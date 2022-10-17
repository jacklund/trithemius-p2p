use futures::future::{FutureExt, Ready};
use libp2p::{multiaddr::Protocol, Multiaddr};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_tcp::tokio::TcpStream;
use std::net::SocketAddr;
use std::task::Poll;
use tokio_socks::{tcp::Socks5Stream, Error as SocksError};

pub fn multiaddr_to_socketaddr(addr: Multiaddr) -> Result<SocketAddr, SocksError> {
    let mut iter = addr.iter();
    let port = match iter.find(|p| matches!(p, Protocol::Tcp(_))) {
        Some(Protocol::Tcp(tcp_port)) => tcp_port,
        _ => return Err(SocksError::AddressTypeNotSupported),
    };
    let mut iter = addr.iter();
    match iter.find(|p| matches!(p, Protocol::Ip4(_) | Protocol::Ip6(_))) {
        Some(Protocol::Ip4(ipv4)) => {
            return Ok(SocketAddr::new(ipv4.into(), port));
        }
        Some(Protocol::Ip6(ipv6)) => {
            return Ok(SocketAddr::new(ipv6.into(), port));
        }
        _ => (),
    }
    Err(SocksError::AddressTypeNotSupported)
}

pub fn multiaddr_to_pair(addr: Multiaddr) -> Result<(String, u16), SocksError> {
    let mut iter = addr.iter();
    let port = match iter.find(|p| matches!(p, Protocol::Tcp(_))) {
        Some(Protocol::Tcp(tcp_port)) => tcp_port,
        _ => return Err(SocksError::AddressTypeNotSupported),
    };
    let mut iter = addr.iter();
    match iter.find(|p| matches!(p, Protocol::Ip4(_) | Protocol::Ip6(_))) {
        Some(Protocol::Ip4(ipv4)) => {
            return Ok((ipv4.to_string(), port));
        }
        Some(Protocol::Ip6(ipv6)) => {
            return Ok((ipv6.to_string(), port));
        }
        _ => (),
    }
    let mut iter = addr.iter();
    match iter.find(|p| matches!(p, Protocol::Dns(_) | Protocol::Dns4(_) | Protocol::Dns6(_))) {
        Some(Protocol::Dns(dns)) => {
            return Ok((dns.into(), port));
        }
        Some(Protocol::Dns4(dns)) => {
            return Ok((dns.into(), port));
        }
        Some(Protocol::Dns6(dns)) => {
            return Ok((dns.into(), port));
        }
        _ => (),
    }
    Err(SocksError::AddressTypeNotSupported)
}

pub struct Socks5Transport {
    proxy_addr: Option<Multiaddr>,
}

impl Default for Socks5Transport {
    fn default() -> Self {
        Self::new()
    }
}

impl Socks5Transport {
    pub fn new() -> Self {
        Self { proxy_addr: None }
    }

    pub fn initialize(&mut self, proxy_addr: Multiaddr) {
        self.proxy_addr = Some(proxy_addr);
    }

    fn do_dial(
        &mut self,
        proxy_addr: Multiaddr,
        addr: Multiaddr,
    ) -> Result<<Self as Transport>::Dial, TransportError<<Self as Transport>::Error>> {
        let proxy_addr = multiaddr_to_socketaddr(proxy_addr).map_err(TransportError::Other)?;
        let socket_addr = match multiaddr_to_pair(addr.clone()) {
            Ok(socket_addr) => socket_addr,
            Err(_) => Err(TransportError::MultiaddrNotSupported(addr))?,
        };
        Ok(async move {
            match Socks5Stream::connect(proxy_addr, socket_addr).await {
                Ok(connection) => Ok(TcpStream(connection.into_inner())),
                Err(error) => Err(error),
            }
        }
        .boxed())
    }
}

impl Transport for Socks5Transport {
    type Output = TcpStream;
    type Error = SocksError;
    type Dial =
        std::pin::Pin<Box<dyn futures::Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = Ready<Result<Self::Output, Self::Error>>;

    fn listen_on(&mut self, address: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        Err(TransportError::MultiaddrNotSupported(address))
    }

    fn remove_listener(&mut self, _id: ListenerId) -> bool {
        false
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let mut working_addr = addr.clone();
        let protocol = working_addr.pop();
        match protocol.clone() {
            Some(Protocol::Socks5(proxy_addr)) => {
                let proxy_addr = if proxy_addr.is_empty() {
                    if self.proxy_addr.is_none() {
                        Err(TransportError::MultiaddrNotSupported(addr.clone()))?;
                    }
                    self.proxy_addr.clone().unwrap()
                } else {
                    proxy_addr
                };
                self.do_dial(proxy_addr, working_addr)
            }
            Some(protocol) => {
                if self.proxy_addr.is_some() {
                    self.do_dial(self.proxy_addr.clone().unwrap(), addr)
                } else {
                    addr.clone().push(protocol);
                    Err(TransportError::MultiaddrNotSupported(addr))
                }
            }
            None => Err(TransportError::MultiaddrNotSupported(addr)),
        }
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.dial(addr)
    }

    fn address_translation(&self, _listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        Some(observed.clone())
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        Poll::Pending
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
    use tokio_socks::Error as SocksError;

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
    ) -> Result<TcpStream, TransportError<SocksError>> {
        let mut transport = Socks5Transport::default();
        if proxy_addr.is_some() {
            transport.initialize(proxy_addr.unwrap());
        }

        transport
            .dial(dial_addr)?
            .await
            .map_err(TransportError::Other)
    }

    async fn test(
        addr: Multiaddr,
        add_proxy: bool,
        use_proxy: bool,
        expected: Result<Multiaddr, TransportError<SocksError>>,
    ) {
        let (ready_tx, mut ready_rx) = mpsc::channel(1);
        let listener = listener(ready_tx);
        let join_handle = tokio::spawn(listener);
        let proxy_addr = ready_rx.next().await.unwrap();
        let addr = if add_proxy {
            let mut new_addr = addr.clone();
            new_addr.push(Protocol::Socks5(proxy_addr.clone()));
            new_addr
        } else {
            addr
        };
        let dialer = if use_proxy {
            dialer(Some(proxy_addr), addr)
        } else {
            dialer(None, addr)
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
    async fn test_socks5_with_proxy() {
        test(
            "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            false,
            true,
            Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
        )
        .await;

        test(
            "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            true,
            true,
            Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
        )
        .await;

        test(
            "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            false,
            true,
            Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
        )
        .await;

        test(
            "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            true,
            true,
            Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
        )
        .await;
    }

    #[tokio::test]
    async fn test_socks5_no_proxy() {
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
            "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
            true,
            false,
            Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
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
            "/dns/www.foo.com/tcp/5678".parse().unwrap(),
            true,
            false,
            Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
        )
        .await;
    }
}
