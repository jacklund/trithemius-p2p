use futures::future::{FutureExt, MapErr, TryFutureExt};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_dns::{DnsErr, TokioDnsConfig};
use libp2p_tcp::{tokio::TcpStream, GenTcpConfig, GenTcpTransport, TokioTcpTransport};
use multiaddr::{Multiaddr, Protocol};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tokio_socks::tcp::Socks5Stream;

#[derive(Debug)]
pub enum TorDnsTransportError {
    Transport(DnsErr<std::io::Error>),
    SocksError(tokio_socks::Error),
    MultiaddrNotSupported(Multiaddr),
}

impl fmt::Display for TorDnsTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        unimplemented!()
    }
}

impl std::error::Error for TorDnsTransportError {}

impl From<std::io::Error> for TorDnsTransportError {
    fn from(error: std::io::Error) -> Self {
        TorDnsTransportError::Transport(DnsErr::Transport(error))
    }
}

impl From<DnsErr<std::io::Error>> for TorDnsTransportError {
    fn from(error: DnsErr<std::io::Error>) -> Self {
        TorDnsTransportError::Transport(error)
    }
}

enum PollNext {
    DnsTcp,
    Tor,
}

pub struct TorDnsTransport {
    dns_tcp: Arc<Mutex<TokioDnsConfig<TokioTcpTransport>>>,
    tor: Arc<Mutex<TokioTcpTransport>>,
    proxy_addr: SocketAddr,
    poll_next: PollNext,
    tor_map: HashMap<u16, Multiaddr>,
}

impl TorDnsTransport {
    pub fn new(proxy_addr: SocketAddr) -> Result<Self, std::io::Error> {
        Ok(Self {
            dns_tcp: Arc::new(Mutex::new(TokioDnsConfig::system(TokioTcpTransport::new(
                GenTcpConfig::default().nodelay(true),
            ))?)),
            tor: Arc::new(Mutex::new(TokioTcpTransport::new(
                GenTcpConfig::default().nodelay(true),
            ))),
            proxy_addr,
            poll_next: PollNext::DnsTcp,
            tor_map: HashMap::new(),
        })
    }

    fn poll_dns_tcp(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<TransportEvent<<Self as Transport>::ListenerUpgrade, <Self as Transport>::Error>>
    {
        let mut dns_tcp = self.dns_tcp.lock();
        Transport::poll(Pin::new(dns_tcp.deref_mut()), cx).map(|event| {
            event
                .map_upgrade(|upgr| upgr.map_err::<_, fn(_) -> _>(TorDnsTransportError::Transport))
                .map_err(TorDnsTransportError::Transport)
        })
    }

    fn poll_tor(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<TransportEvent<<Self as Transport>::ListenerUpgrade, <Self as Transport>::Error>>
    {
        let mut tor = self.tor.lock();
        // TODO: Catch address events and convert them
        let mut event = Transport::poll(Pin::new(tor.deref_mut()), cx).map(|event| {
            event
                .map_upgrade(|upgr| upgr.map_err::<_, fn(_) -> _>(DnsErr::Transport))
                .map_upgrade(|upgr| upgr.map_err::<_, fn(_) -> _>(TorDnsTransportError::Transport))
                .map_err(DnsErr::Transport)
                .map_err(TorDnsTransportError::Transport)
        });

        match event {
            Poll::Ready(TransportEvent::NewAddress {
                listener_id,
                ref mut listen_addr,
            }) => loop {
                if let Some(Protocol::Tcp(port)) = listen_addr.pop() {
                    match self.tor_map.get(&port) {
                        Some(tor_addr) => {
                            return Poll::Ready(TransportEvent::NewAddress {
                                listener_id,
                                listen_addr: tor_addr.clone(),
                            });
                        }
                        None => {
                            return event;
                        }
                    }
                }
            },
            Poll::Ready(TransportEvent::AddressExpired {
                listener_id,
                ref mut listen_addr,
            }) => loop {
                if let Some(Protocol::Tcp(port)) = listen_addr.pop() {
                    match self.tor_map.get(&port) {
                        Some(tor_addr) => {
                            return Poll::Ready(TransportEvent::AddressExpired {
                                listener_id,
                                listen_addr: tor_addr.clone(),
                            });
                        }
                        None => {
                            return event;
                        }
                    }
                }
            },
            Poll::Ready(TransportEvent::Incoming {
                listener_id,
                upgrade,
                local_addr,
                ref send_back_addr,
            }) => loop {
                let mut address = local_addr.clone();
                if let Some(Protocol::Tcp(port)) = address.pop() {
                    match self.tor_map.get(&port) {
                        Some(tor_addr) => {
                            return Poll::Ready(TransportEvent::Incoming {
                                listener_id,
                                upgrade,
                                local_addr: tor_addr.clone(),
                                send_back_addr: Multiaddr::empty(),
                            });
                        }
                        None => {
                            return Poll::Ready(TransportEvent::Incoming {
                                listener_id,
                                upgrade,
                                local_addr,
                                send_back_addr: send_back_addr.clone(),
                            });
                        }
                    }
                }
            },
            _ => event,
        }
    }
}

fn to_onion3_domain(mut addr: Multiaddr) -> Option<(String, u16)> {
    if let Some(Protocol::Onion3(address)) = addr.pop() {
        let onion_address = format!(
            "{}.onion",
            base32::encode(base32::Alphabet::RFC4648 { padding: false }, address.hash())
        );
        Some((onion_address, address.port()))
    } else {
        None
    }
}

impl Transport for TorDnsTransport {
    type Output = TcpStream;
    type Error = TorDnsTransportError;
    type Dial =
        std::pin::Pin<Box<dyn futures::Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = MapErr<
        <TokioDnsConfig<GenTcpTransport<libp2p_tcp::tokio::Tcp>> as Transport>::ListenerUpgrade,
        fn(
            <TokioDnsConfig<GenTcpTransport<libp2p_tcp::tokio::Tcp>> as Transport>::Error,
        ) -> Self::Error,
    >;

    fn listen_on(&mut self, address: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        let mut local_addr = Multiaddr::empty();
        for protocol in address.iter() {
            match protocol {
                Protocol::Onion3(addr) => {
                    let mut tor_addr: Multiaddr = "/ip4/127.0.0.1".parse().unwrap();
                    tor_addr.push(Protocol::Tcp(addr.port()));
                    let result = self.tor.lock().listen_on(tor_addr).map_err(|e| match e {
                        TransportError::Other(std_error) => TransportError::Other(std_error.into()),
                        TransportError::MultiaddrNotSupported(addr) => {
                            TransportError::MultiaddrNotSupported(addr)
                        }
                    });
                    if result.is_ok() {
                        self.tor_map.insert(addr.port(), address);
                    };
                    return result;
                }
                _ => {
                    local_addr.push(protocol);
                }
            }
        }
        self.dns_tcp
            .lock()
            .listen_on(local_addr)
            .map_err(|e| match e {
                TransportError::Other(dnserr) => TransportError::Other(dnserr.into()),
                TransportError::MultiaddrNotSupported(addr) => {
                    TransportError::MultiaddrNotSupported(addr)
                }
            })
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.dns_tcp.lock().remove_listener(id) || self.tor.lock().remove_listener(id)
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let proxy_addr = self.proxy_addr;
        let dns_tcp = self.dns_tcp.clone();
        Ok(async move {
            if let Some((host, port)) = to_onion3_domain(addr.clone()) {
                match Socks5Stream::connect(proxy_addr, (host, port)).await {
                    Ok(connection) => Ok(TcpStream(connection.into_inner())),
                    Err(error) => Err(TorDnsTransportError::SocksError(error)),
                }
            } else {
                let dial = dns_tcp.lock().dial(addr);
                match dial {
                    Ok(future) => future.await.map_err(|e| e.into()),
                    Err(error) => match error {
                        TransportError::MultiaddrNotSupported(addr) => {
                            Err(TorDnsTransportError::MultiaddrNotSupported(addr))
                        }
                        TransportError::Other(error) => Err(error.into()),
                    },
                }
            }
        }
        .boxed())
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        let dns_tcp = self.dns_tcp.clone();
        Ok(async move {
            let dial = dns_tcp.lock().dial_as_listener(addr);
            match dial {
                Ok(future) => future.await.map_err(|e| e.into()),
                Err(error) => match error {
                    TransportError::MultiaddrNotSupported(addr) => {
                        Err(TorDnsTransportError::MultiaddrNotSupported(addr))
                    }
                    TransportError::Other(error) => Err(error.into()),
                },
            }
        }
        .boxed())
    }

    fn address_translation(&self, listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        self.dns_tcp.lock().address_translation(listen, observed)
    }

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        match self.poll_next {
            PollNext::DnsTcp => {
                let event = self.as_mut().poll_dns_tcp(cx);
                match event {
                    Poll::Pending => (),
                    _ => {
                        self.as_mut().poll_next = PollNext::Tor;
                        return event;
                    }
                }

                self.poll_tor(cx)
            }
            PollNext::Tor => {
                let event = self.as_mut().poll_tor(cx);
                match event {
                    Poll::Pending => (),
                    _ => {
                        self.as_mut().poll_next = PollNext::DnsTcp;
                        return event;
                    }
                }

                self.poll_dns_tcp(cx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures;
    use libp2p_tcp::{GenTcpConfig, TokioTcpTransport};
    use socks5_proto::{Address, Reply};
    use socks5_server::{auth::NoAuth, Connection, Server};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn test_tor_connect() -> Result<(), Box<dyn std::error::Error>> {
        let server = Server::bind("127.0.0.1:5000", Arc::new(NoAuth))
            .await
            .unwrap();

        tokio::spawn(async move {
            while let Ok((conn, _)) = server.accept().await {
                match conn.handshake().await.unwrap() {
                    Connection::Connect(connect, _addr) => {
                        let mut conn = connect
                            .reply(Reply::Succeeded, Address::unspecified())
                            .await
                            .unwrap();
                        let mut buf = [0; 80];
                        conn.read(&mut buf[..]).await.unwrap();
                        conn.write("Pong!".as_bytes()).await.unwrap();
                    }
                    _ => panic!("Whaaaa?"),
                }
            }
        });

        let mut transport = TorTransportWrapper::new(
            TokioTcpTransport::new(GenTcpConfig::default().nodelay(true)),
            "127.0.0.1:5000".parse().unwrap(),
        )?;

        let mut stream = transport
            .dial(
                "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234"
                    .parse()
                    .unwrap(),
            )?
            .await?;

        futures::AsyncWriteExt::write(&mut stream, "Ping!".as_bytes()).await?;
        let mut buf = [0; 80];
        let bytes_read = futures::AsyncReadExt::read(&mut stream, &mut buf).await?;

        assert_eq!("Pong!", std::str::from_utf8(&buf[..bytes_read])?);

        Ok(())
    }
}
