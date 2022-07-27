use async_socks5::{connect, AddrKind, Auth};
use futures::future::FutureExt;
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_tcp::TokioTcpTransport;
use multiaddr::{Multiaddr, Protocol};
use std::io::{Error, ErrorKind};
use std::net::SocketAddr;

pub struct SOCKS5Transport {
    proxy_addr: Multiaddr,
    auth: Option<Auth>,
}

impl SOCKS5Transport {
    pub fn new(proxy_addr: Multiaddr, auth: Option<Auth>) -> Self {
        Self { proxy_addr, auth }
    }
}

fn convert_multiaddr(mut addr: Multiaddr) -> AddrKind {
    match addr.pop() {
        Some(Protocol::Tcp(port)) => match addr.pop() {
            Some(Protocol::Ip4(ipv4)) => AddrKind::Ip(SocketAddr::new(ipv4.into(), port)),
            Some(Protocol::Ip6(ipv6)) => AddrKind::Ip(SocketAddr::new(ipv6.into(), port)),
            Some(Protocol::Dns(host)) => AddrKind::Domain(host.to_string(), port),
            _ => panic!("Bad address"),
        },
        Some(Protocol::Onion3(address)) => {
            let onion_address = format!(
                "{}.onion",
                base32::encode(base32::Alphabet::RFC4648 { padding: false }, address.hash())
            );
            AddrKind::Domain(onion_address, address.port())
        }
        _ => panic!("Bad address"),
    }
}

impl Transport for SOCKS5Transport {
    type Output = libp2p_tcp::tokio::TcpStream;
    type Error = std::io::Error;
    type Dial =
        std::pin::Pin<Box<dyn futures::Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = futures::future::Ready<Result<Self::Output, Self::Error>>;

    fn listen_on(&mut self, _addr: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        unimplemented!()
    }

    fn remove_listener(&mut self, _id: ListenerId) -> bool {
        unimplemented!()
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let dest_addr = addr;
        let proxy_addr = self.proxy_addr.clone();
        let auth = self.auth.clone();
        Ok(async move {
            match TokioTcpTransport::default().dial(proxy_addr) {
                Ok(future) => match future.await {
                    Ok(mut stream) => {
                        match connect(&mut stream.0, convert_multiaddr(dest_addr), auth).await {
                            Ok(_) => Ok(stream),
                            Err(error) => {
                                return Err(Error::new(ErrorKind::Other, error.to_string()));
                            }
                        }
                    }
                    Err(error) => return Err(error),
                },
                Err(TransportError::MultiaddrNotSupported(_)) => {
                    return Err(Error::new(ErrorKind::Other, "Multiaddr not supported"))
                }
                Err(TransportError::Other(error)) => {
                    return Err(error);
                }
            }
        }
        .boxed())
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.dial(addr)
    }

    fn address_translation(&self, _listen: &Multiaddr, _observed: &Multiaddr) -> Option<Multiaddr> {
        unimplemented!()
    }

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures;
    use libp2p::Multiaddr;
    use socks5_proto::{Address, Reply};
    use socks5_server::{auth::NoAuth, Connection, Server};
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn test_socks5_connect() -> Result<(), Box<dyn std::error::Error>> {
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

        let mut transport = SOCKS5Transport::new(
            Multiaddr::from_str("/ip4/127.0.0.1/tcp/5000").unwrap(),
            None,
        );
        let mut stream = transport
            .dial(Multiaddr::from_str("/dns/www.google.com/tcp/80").unwrap())?
            .await?;

        futures::AsyncWriteExt::write(&mut stream, "Ping!".as_bytes()).await?;
        let mut buf = [0; 80];
        let bytes_read = futures::AsyncReadExt::read(&mut stream, &mut buf).await?;

        assert_eq!("Pong!", std::str::from_utf8(&buf[..bytes_read])?);

        Ok(())
    }
}
