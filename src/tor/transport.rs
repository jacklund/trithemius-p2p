use async_socks5::{connect, AddrKind, Auth};
use futures::future::{Future, FutureExt, MapErr, TryFutureExt};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use multiaddr::{Multiaddr, Protocol};
use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug)]
pub enum TorTransportError<TErr> {
    SOCKS5Error(async_socks5::Error),
    Transport(TErr),
    MultiaddrNotSupported(Multiaddr),
}

impl<TErr> fmt::Display for TorTransportError<TErr>
where
    TErr: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TorTransportError::SOCKS5Error(err) => write!(f, "{}", err),
            TorTransportError::Transport(err) => write!(f, "{}", err),
            TorTransportError::MultiaddrNotSupported(a) => {
                write!(f, "Unsupported resolved address: {}", a)
            }
        }
    }
}

impl<TErr> std::error::Error for TorTransportError<TErr> where TErr: fmt::Debug + fmt::Display {}

pub struct TorTransportWrapper<T> {
    inner: T,
    proxy_addr: Multiaddr,
    auth: Option<Auth>,
}

impl<T> TorTransportWrapper<T> {
    pub fn new(
        inner: T,
        proxy_addr: Multiaddr,
        auth: Option<Auth>,
    ) -> Result<TorTransportWrapper<T>, Box<dyn std::error::Error>> {
        Ok(Self {
            inner,
            proxy_addr,
            auth,
        })
    }
}

fn convert_multiaddr<T>(mut addr: Multiaddr) -> Result<AddrKind, TorTransportError<T>> {
    match addr.pop() {
        Some(Protocol::Tcp(port)) => match addr.pop() {
            Some(Protocol::Ip4(ipv4)) => Ok(AddrKind::Ip(SocketAddr::new(ipv4.into(), port))),
            Some(Protocol::Ip6(ipv6)) => Ok(AddrKind::Ip(SocketAddr::new(ipv6.into(), port))),
            Some(Protocol::Dns(host)) => Ok(AddrKind::Domain(host.to_string(), port)),
            _ => Err(TorTransportError::MultiaddrNotSupported(addr)),
        },
        Some(Protocol::Onion3(address)) => {
            let onion_address = format!(
                "{}.onion",
                base32::encode(base32::Alphabet::RFC4648 { padding: false }, address.hash())
            );
            Ok(AddrKind::Domain(onion_address, address.port()))
        }
        _ => Err(TorTransportError::MultiaddrNotSupported(addr)),
    }
}

impl<T> Transport for TorTransportWrapper<T>
where
    T: Transport + Send + Clone + Unpin + 'static,
    T::Output: AsyncRead + AsyncWrite + Send + Unpin,
    T::Dial: Send,
    T::Error: Send,
{
    type Output = T::Output;
    type Error = TorTransportError<T::Error>;
    type Dial =
        std::pin::Pin<Box<dyn futures::Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = MapErr<T::ListenerUpgrade, fn(T::Error) -> Self::Error>;

    fn listen_on(&mut self, addr: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        self.inner
            .listen_on(addr)
            .map_err(|e| e.map(TorTransportError::Transport))
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let dest_addr = addr.clone();
        let proxy_addr = self.proxy_addr.clone();
        let auth = self.auth.clone();
        let mut inner = self.inner.clone();
        Ok(async move {
            let addr_kind = convert_multiaddr(dest_addr.clone())?;
            if let AddrKind::Domain(host, _port) = addr_kind.clone() {
                if host.ends_with(".onion") {
                    match inner.dial(proxy_addr) {
                        Ok(future) => match future.await {
                            Ok(mut stream) => match connect(&mut stream, addr_kind, auth).await {
                                Ok(_) => Ok(stream),
                                Err(error) => {
                                    return Err(TorTransportError::SOCKS5Error(error));
                                }
                            },
                            Err(error) => {
                                return Err(TorTransportError::Transport(error));
                            }
                        },
                        Err(TransportError::MultiaddrNotSupported(dest_addr)) => {
                            return Err(TorTransportError::MultiaddrNotSupported(dest_addr));
                        }
                        Err(TransportError::Other(error)) => {
                            return Err(TorTransportError::Transport(error));
                        }
                    }
                } else {
                    match inner.dial(dest_addr) {
                        Ok(future) => future.await.map_err(|e| TorTransportError::Transport(e)),
                        Err(error) => match error {
                            TransportError::MultiaddrNotSupported(dest_addr) => {
                                Err(TorTransportError::MultiaddrNotSupported(dest_addr))
                            }
                            TransportError::Other(error) => {
                                Err(TorTransportError::Transport(error))
                            }
                        },
                    }
                }
            } else {
                match inner.dial(dest_addr) {
                    Ok(future) => future.await.map_err(|e| TorTransportError::Transport(e)),
                    Err(error) => match error {
                        TransportError::MultiaddrNotSupported(dest_addr) => {
                            Err(TorTransportError::MultiaddrNotSupported(dest_addr))
                        }
                        TransportError::Other(error) => Err(TorTransportError::Transport(error)),
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
        let mut inner = self.inner.clone();
        Ok(async move {
            match inner.dial_as_listener(addr) {
                Ok(future) => future.await.map_err(TorTransportError::Transport),
                Err(error) => match error {
                    TransportError::MultiaddrNotSupported(addr) => {
                        Err(TorTransportError::MultiaddrNotSupported(addr))
                    }
                    TransportError::Other(error) => Err(TorTransportError::Transport(error)),
                },
            }
        }
        .boxed())
    }

    fn address_translation(&self, listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        self.inner.address_translation(listen, observed)
    }

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        Transport::poll(Pin::new(&mut self.inner), cx).map(|event| {
            event
                .map_upgrade(|upgr| upgr.map_err::<_, fn(_) -> _>(TorTransportError::Transport))
                .map_err(TorTransportError::Transport)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures;
    use socks5_proto::{Address, Reply};
    use socks5_server::{auth::NoAuth, Connection, Server};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

        let mut transport = TorTransport::new("/ip4/127.0.0.1/tcp/5000".parse().unwrap(), None);
        let mut stream = transport
            .dial("/dns/www.google.com/tcp/80".parse().unwrap())?
            .await?;

        futures::AsyncWriteExt::write(&mut stream, "Ping!".as_bytes()).await?;
        let mut buf = [0; 80];
        let bytes_read = futures::AsyncReadExt::read(&mut stream, &mut buf).await?;

        assert_eq!("Pong!", std::str::from_utf8(&buf[..bytes_read])?);

        Ok(())
    }
}
