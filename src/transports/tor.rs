use crate::tor::control_connection::OnionService;
use crate::tor::error::TorError;
use crate::transports::socks5::Socks5Transport;
use futures::future::{BoxFuture, Ready};
use futures::{FutureExt, TryFutureExt};
use lazy_static::lazy_static;
use libp2p::{multiaddr::Protocol, Multiaddr};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_tcp::tokio::TcpStream;
use libp2p_tcp::{GenTcpConfig, TokioTcpTransport};
use std::pin::Pin;
use std::task::Poll;
use tokio::sync::mpsc;

lazy_static! {
    static ref DEFAULT_TOR_PROXY_ADDRESS: Multiaddr = "/ip4/127.0.0.1/tcp/9050".parse().unwrap();
}

pub struct TorTransport {
    outbound: Option<Socks5Transport>,
    inbound: TokioTcpTransport,
    onion_service_channel: Option<mpsc::Receiver<OnionService>>,
    onion_services: Vec<OnionService>,
    always_use: bool,
}

fn transform_socks_error(e: TransportError<tokio_socks::Error>) -> TransportError<TorError> {
    match e {
        TransportError::MultiaddrNotSupported(addr) => TransportError::MultiaddrNotSupported(addr),
        TransportError::Other(socks_error) => {
            TransportError::Other(TorError::Socks5Error(socks_error))
        }
    }
}

fn transform_stdio_error(e: TransportError<std::io::Error>) -> TransportError<TorError> {
    match e {
        TransportError::MultiaddrNotSupported(addr) => TransportError::MultiaddrNotSupported(addr),
        TransportError::Other(io_error) => TransportError::Other(TorError::IOError(io_error)),
    }
}

// Dial:
// - /dns/www.google.com/https/tor -> /dns/www.google.com/socks5/ip4/127.0.0.1/tcp/9050
// - /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234 ->
//      /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234/socks5/ip4/127.0.0.1/tcp/9050
// Listen:
// - /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234 ->
//      /ip4/127.0.0.1/tcp/1234
impl TorTransport {
    pub fn new() -> Self {
        Self {
            outbound: None,
            inbound: TokioTcpTransport::new(GenTcpConfig::default().nodelay(true)),
            onion_service_channel: None,
            onion_services: Vec::new(),
            always_use: false,
        }
    }

    pub fn use_onion_service_channel(mut self, rx: mpsc::Receiver<OnionService>) -> Self {
        self.onion_service_channel = Some(rx);

        self
    }

    pub fn use_proxy_address(mut self, proxy_addr: Multiaddr) -> Self {
        self.outbound = Some(
            Socks5Transport::new()
                .default_proxy_address(proxy_addr)
                .always_use(),
        );

        self
    }

    pub fn use_default_proxy_address(self) -> Self {
        self.use_proxy_address(DEFAULT_TOR_PROXY_ADDRESS.clone())
    }

    pub fn always_use(mut self) -> Self {
        self.always_use = true;

        self
    }

    fn do_dial(
        &mut self,
        mut addr: Multiaddr,
    ) -> Result<
        impl futures::Future<Output = Result<TcpStream, tokio_socks::Error>>,
        TransportError<tokio_socks::Error>,
    > {
        if self.always_use
            && addr
                .iter()
                .find(|p| matches!(p, Protocol::Tor | Protocol::Onion3(_)))
                .is_none()
        {
            addr.push(Protocol::Tor);
        }

        if self.outbound.is_some() {
            let mut iter = addr.iter();
            match iter.find(|p| matches!(p, Protocol::Tor | Protocol::Onion3(_))) {
                // /dns/www.google.com/https/tor -> /dns/www.google.com/socks5/ip4/127.0.0.1/tcp/9050
                Some(Protocol::Tor) => {
                    let mut new_addr = Multiaddr::empty();
                    for p in addr.iter() {
                        if p == Protocol::Tor {
                            new_addr.push(Protocol::Socks5(Multiaddr::empty()));
                        } else {
                            new_addr.push(p);
                        }
                    }
                    self.outbound.as_mut().unwrap().do_dial(new_addr)
                }

                // /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234 ->
                //      /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234/socks5/ip4/127.0.0.1/tcp/9050
                Some(Protocol::Onion3(address)) => {
                    let mut new_addr = Multiaddr::empty();
                    // Remove the Onion3 address
                    for p in addr.iter().filter(|p| !matches!(p, Protocol::Onion3(_))) {
                        new_addr.push(p);
                    }
                    let onion_address = format!(
                        "{}.onion",
                        base32::encode(
                            base32::Alphabet::RFC4648 { padding: false },
                            address.hash()
                        )
                        .to_ascii_lowercase()
                    );
                    // Push the onion address and port for Socks5 to consume
                    new_addr.push(Protocol::Dns(std::borrow::Cow::Borrowed(&onion_address)));
                    new_addr.push(Protocol::Tcp(address.port()));
                    self.outbound.as_mut().unwrap().do_dial(new_addr)
                }
                _ => Err(TransportError::MultiaddrNotSupported(addr)),
            }
        } else {
            Err(TransportError::MultiaddrNotSupported(addr))
        }
    }
}

impl Transport for TorTransport {
    type Output = TcpStream;
    type Error = TorError;
    type Dial = BoxFuture<'static, Result<TcpStream, TorError>>;
    type ListenerUpgrade = Ready<Result<Self::Output, Self::Error>>;

    // NOTE: We can't bind/listen on the Tor SOCKS proxy, so we can't listen for /tor
    // addresses; however we do listen on a local port (or unix socket) for Onion3 addresses
    fn listen_on(&mut self, addr: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        if self.onion_service_channel.is_some() {
            while let Ok(onion_service) = self.onion_service_channel.as_mut().unwrap().try_recv() {
                self.onion_services.push(onion_service);
            }

            if let Some(onion_service) = self
                .onion_services
                .iter()
                .find(|o| o.listen_address == addr)
            {
                Ok(self
                    .inbound
                    .listen_on(onion_service.listen_address.clone())
                    .map_err(transform_stdio_error)?)
            } else {
                Err(TransportError::MultiaddrNotSupported(addr))
            }
        } else {
            Err(TransportError::MultiaddrNotSupported(addr))
        }
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inbound.remove_listener(id)
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        Ok(self
            .do_dial(addr)
            .map(|f| f.err_into())
            .map_err(|e| transform_socks_error(e))?
            .boxed())
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.dial(addr)
    }

    fn address_translation(&self, _listen: &Multiaddr, _observed: &Multiaddr) -> Option<Multiaddr> {
        None
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        let me = self.get_mut();
        let poll = Pin::new(&mut me.inbound).poll(cx);
        if let Poll::Ready(TransportEvent::NewAddress {
            listener_id,
            listen_addr,
        }) = poll
        {
            if let Some(onion_service) = me
                .onion_services
                .iter()
                .find(|o| o.listen_address == listen_addr)
            {
                Poll::Ready(TransportEvent::NewAddress {
                    listener_id,
                    listen_addr: onion_service.address.clone(),
                })
            } else {
                Poll::Pending
            }
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::mpsc;
    use futures::{SinkExt, StreamExt};
    use socks5_proto::{Address, Reply};
    use socks5_server::{auth::NoAuth, Connection, Server};
    use std::sync::Arc;

    async fn handle(conn: Connection) -> Result<Option<Multiaddr>, std::io::Error> {
        match conn {
            Connection::Connect(connect, addr) => {
                let mut multiaddr = Multiaddr::empty();
                match addr {
                    Address::SocketAddress(socket_addr) => {
                        multiaddr.push(socket_addr.ip().into());
                        multiaddr.push(Protocol::Tcp(socket_addr.port()));
                    }
                    Address::DomainAddress(domain, port) => {
                        multiaddr.push(Protocol::Dns(std::borrow::Cow::Borrowed(&domain)));
                        multiaddr.push(Protocol::Tcp(port));
                    }
                };
                let mut conn = connect
                    .reply(Reply::Succeeded, Address::unspecified())
                    .await?;
                conn.shutdown().await?;
                return Ok(Some(multiaddr));
            }
            _ => panic!("This shouldn't happen"),
        }
    }

    async fn listener(mut ready_tx: mpsc::Sender<Multiaddr>) -> Option<Multiaddr> {
        let server = Server::bind("127.0.0.1:0", Arc::new(NoAuth)).await.unwrap();
        let local_addr = server.local_addr().unwrap();
        let listen_addr: Multiaddr = format!("/ip4/{}/tcp/{}", local_addr.ip(), local_addr.port())
            .parse()
            .unwrap();
        ready_tx.send(listen_addr).await.unwrap();
        let (conn, _) = server.accept().await.unwrap();
        let connection = conn.handshake().await.unwrap();
        match handle(connection).await {
            Ok(Some(addr)) => Some(addr),
            Ok(None) => None,
            Err(_) => None,
        }
    }

    async fn dialer(
        proxy_addr: Option<Multiaddr>,
        dial_addr: Multiaddr,
        always_use: bool,
    ) -> Result<TcpStream, TransportError<TorError>> {
        let mut transport = TorTransport::new();
        if proxy_addr.is_some() {
            transport = transport.use_proxy_address(proxy_addr.unwrap());
        }
        if always_use {
            transport = transport.always_use();
        }

        transport
            .dial(dial_addr)?
            .await
            .map_err(TransportError::Other)
    }

    async fn test(
        addr: Multiaddr,
        use_proxy: bool,
        always_use: bool,
        expected: Result<Multiaddr, TransportError<TorError>>,
    ) {
        let (ready_tx, mut ready_rx) = mpsc::channel(1);
        let listener = listener(ready_tx);
        let join_handle = tokio::spawn(listener);
        let proxy_addr = ready_rx.next().await.unwrap();
        let dialer = if use_proxy {
            dialer(Some(proxy_addr), addr, always_use)
        } else {
            dialer(None, addr, always_use)
        };
        let result = dialer.await;
        let actual_addr_opt = if expected.is_ok() {
            join_handle.await.unwrap()
        } else {
            join_handle.abort();
            None
        };
        if expected.is_err() {
            match expected {
                Err(TransportError::MultiaddrNotSupported(addr1)) => {
                    assert!(result.is_err());
                    match result {
                        Err(TransportError::MultiaddrNotSupported(addr2)) => {
                            assert_eq!(addr1, addr2);
                        }
                        _ => assert!(false),
                    }
                }
                _ => assert!(false),
            }
        } else {
            assert!(actual_addr_opt.is_some());
            assert_eq!(expected.unwrap(), actual_addr_opt.unwrap());
        }
    }

    #[tokio::test]
    async fn test_tor_with_proxy() {
        test(
            "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            true,
            false,
            Err(TransportError::MultiaddrNotSupported(
                "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            )),
        )
        .await;

        test(
            "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            true,
            false,
            Err(TransportError::MultiaddrNotSupported(
                "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            )),
        )
        .await;

        test(
            "/ip4/10.11.12.13/tcp/80/tor".parse().unwrap(),
            true,
            false,
            Ok("/ip4/10.11.12.13/tcp/80".parse().unwrap()),
        )
        .await;

        test(
            "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234"
                .parse()
                .unwrap(),
            true,
            false,
            Ok(
                "/dns/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion/tcp/1234"
                    .parse()
                    .unwrap(),
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn test_tor_with_no_proxy() {
        test(
            "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            false,
            false,
            Err(TransportError::MultiaddrNotSupported(
                "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            )),
        )
        .await;

        test(
            "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            false,
            false,
            Err(TransportError::MultiaddrNotSupported(
                "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            )),
        )
        .await;

        test(
            "/ip4/10.11.12.13/tcp/80/tor".parse().unwrap(),
            false,
            false,
            Err(TransportError::MultiaddrNotSupported(
                "/ip4/10.11.12.13/tcp/80/tor".parse().unwrap(),
            )),
        )
        .await;

        test(
            "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234"
                .parse()
                .unwrap(),
            false,
            false,
            Err(TransportError::MultiaddrNotSupported(
                "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234"
                    .parse()
                    .unwrap(),
            )),
        )
        .await;
    }

    #[tokio::test]
    async fn test_tor_always_use() {
        test(
            "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            true,
            true,
            Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
        )
        .await;

        test(
            "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            true,
            true,
            Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
        )
        .await;

        test(
            "/ip4/10.11.12.13/tcp/80/tor".parse().unwrap(),
            true,
            true,
            Ok("/ip4/10.11.12.13/tcp/80".parse().unwrap()),
        )
        .await;

        test(
            "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234"
                .parse()
                .unwrap(),
            true,
            true,
            Ok(
                "/dns/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion/tcp/1234"
                    .parse()
                    .unwrap(),
            ),
        )
        .await;
    }
}
