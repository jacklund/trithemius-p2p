use futures::future::{FutureExt, Ready};
use libp2p::{multiaddr::Protocol, Multiaddr};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_tcp::tokio::TcpStream;
use std::net::SocketAddr;
use std::task::Poll;
use tokio_socks::{tcp::Socks5Stream, Error as SocksError};

pub fn multiaddr_to_socketaddr(mut addr: Multiaddr) -> Result<SocketAddr, SocksError> {
    let mut port = None;
    while let Some(proto) = addr.pop() {
        match proto {
            Protocol::Ip4(ipv4) => match port {
                Some(port) => return Ok(SocketAddr::new(ipv4.into(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Ip6(ipv6) => match port {
                Some(port) => return Ok(SocketAddr::new(ipv6.into(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Tcp(portnum) => match port {
                Some(_) => return Err(SocksError::AddressTypeNotSupported),
                None => port = Some(portnum),
            },
            Protocol::P2p(_) => {}
            _ => return Err(SocksError::AddressTypeNotSupported),
        }
    }
    Err(SocksError::AddressTypeNotSupported)
}

pub fn multiaddr_to_pair(mut addr: Multiaddr) -> Result<(String, u16), SocksError> {
    let mut port = None;
    while let Some(proto) = addr.pop() {
        match proto {
            Protocol::Ip4(ipv4) => match port {
                Some(port) => return Ok((ipv4.to_string(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Ip6(ipv6) => match port {
                Some(port) => return Ok((ipv6.to_string(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Dns(dns) => match port {
                Some(port) => return Ok((dns.into(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Dns4(dns) => match port {
                Some(port) => return Ok((dns.into(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Dns6(dns) => match port {
                Some(port) => return Ok((dns.into(), port)),
                None => return Err(SocksError::AddressTypeNotSupported),
            },
            Protocol::Tcp(portnum) => match port {
                Some(_) => return Err(SocksError::AddressTypeNotSupported),
                None => port = Some(portnum),
            },
            Protocol::P2p(_) => {}
            _ => return Err(SocksError::AddressTypeNotSupported),
        }
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
    use socks5_proto::{Address, Reply};
    use socks5_server::{auth::NoAuth, Connection, Server};
    use std::sync::Arc;
    use tokio::sync::mpsc;
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

    struct Tester {
        transport: Socks5Transport,
        address_rx: mpsc::Receiver<Multiaddr>,
    }

    impl Tester {
        async fn test(
            &mut self,
            addr: Multiaddr,
            expected: Result<Multiaddr, TransportError<SocksError>>,
        ) {
            match expected {
                Ok(address) => match self.transport.dial(addr) {
                    Ok(future) => {
                        future.await.unwrap();
                        let actual_address = self.address_rx.recv().await.unwrap();
                        assert_eq!(address, actual_address);
                    }
                    Err(error) => {
                        println!("Error: {:?}", error);
                        assert!(false);
                    }
                },
                Err(error) => match self.transport.dial(addr) {
                    Ok(_) => {
                        println!("Got success when expecting {:?}", error);
                        assert!(false);
                    }
                    Err(actual_error) => match error {
                        TransportError::MultiaddrNotSupported(expected_addr) => {
                            match actual_error {
                                TransportError::MultiaddrNotSupported(actual_addr) => {
                                    assert_eq!(expected_addr, actual_addr)
                                }
                                _ => assert!(false),
                            }
                        }
                        _ => assert!(false),
                    },
                },
            }
        }
    }

    async fn setup() -> (
        Socks5Transport,
        Multiaddr,
        mpsc::Sender<bool>,
        mpsc::Receiver<Multiaddr>,
        tokio::task::JoinHandle<()>,
    ) {
        let server = Server::bind("127.0.0.1:0", Arc::new(NoAuth)).await.unwrap();
        let local_addr = server.local_addr().unwrap();
        let listen_addr: Multiaddr = format!("/ip4/{}/tcp/{}", local_addr.ip(), local_addr.port())
            .parse()
            .unwrap();
        let (address_tx, address_rx) = mpsc::channel(1);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<bool>(1);
        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = server.accept() => {
                        let (conn, _) = result.unwrap();
                        let connection = conn.handshake().await.unwrap();
                        match handle(connection).await {
                            Ok(Some(addr)) => {
                                address_tx.send(addr).await.unwrap();
                            },
                            Ok(None) => (),
                            Err(_) => (),
                        }
                    },
                    _ = shutdown_rx.recv() => break,
                };
            }
        });
        (
            Socks5Transport::default(),
            listen_addr,
            shutdown_tx,
            address_rx,
            join_handle,
        )
    }

    async fn teardown(shutdown_tx: mpsc::Sender<bool>, join_handle: tokio::task::JoinHandle<()>) {
        shutdown_tx.send(true).await.unwrap();
        join_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_socks5_with_proxy() {
        let (mut transport, listen_addr, shutdown_tx, address_rx, join_handle) = setup().await;
        transport.initialize(listen_addr.clone());

        let mut tester = Tester {
            transport,
            address_rx,
        };

        // Send IP address with and without explicit proxy
        tester
            .test(
                format!("/ip4/1.2.3.4/tcp/5678/socks5{}", listen_addr)
                    .as_str()
                    .parse()
                    .unwrap(),
                Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
            )
            .await;

        tester
            .test(
                "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
                Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
            )
            .await;

        // Send DNS address with and without explicit proxy
        tester
            .test(
                format!("/dns/www.foo.com/tcp/5678/socks5{}", listen_addr)
                    .as_str()
                    .parse()
                    .unwrap(),
                Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
            )
            .await;

        tester
            .test(
                "/dns/www.foo.com/tcp/5678".parse().unwrap(),
                Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
            )
            .await;

        teardown(shutdown_tx, join_handle).await;
    }

    #[tokio::test]
    async fn test_socks5_no_proxy() {
        let (transport, listen_addr, shutdown_tx, address_rx, join_handle) = setup().await;

        let mut tester = Tester {
            transport,
            address_rx,
        };

        // Send IP address with and without explicit proxy
        // Without explicit proxy should fail since we don't have a proxy configured
        tester
            .test(
                format!("/ip4/1.2.3.4/tcp/5678/socks5{}", listen_addr)
                    .as_str()
                    .parse()
                    .unwrap(),
                Ok("/ip4/1.2.3.4/tcp/5678".parse().unwrap()),
            )
            .await;

        tester
            .test(
                "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
                Err(TransportError::MultiaddrNotSupported(
                    "/ip4/1.2.3.4/tcp/5678".parse().unwrap(),
                )),
            )
            .await;

        // Send DNS address with and without explicit proxy
        tester
            .test(
                format!("/dns/www.foo.com/tcp/5678/socks5{}", listen_addr)
                    .as_str()
                    .parse()
                    .unwrap(),
                Ok("/dns/www.foo.com/tcp/5678".parse().unwrap()),
            )
            .await;

        tester
            .test(
                "/dns/www.foo.com/tcp/5678".parse().unwrap(),
                Err(TransportError::MultiaddrNotSupported(
                    "/dns/www.foo.com/tcp/5678".parse().unwrap(),
                )),
            )
            .await;

        teardown(shutdown_tx, join_handle).await;
    }
}
