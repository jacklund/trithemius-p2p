use futures::future::{FutureExt, MapErr, TryFutureExt};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_tcp::tokio::TcpStream;
use multiaddr::{Multiaddr, Protocol};
use parking_lot::Mutex;
use std::fmt;
use std::net::SocketAddr;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync::Arc;
use tokio_socks::tcp::Socks5Stream;

#[derive(Debug)]
pub enum TorTransportError<TErr> {
    Transport(TErr),
    MultiaddrNotSupported(Multiaddr),
    SocksError(tokio_socks::Error),
}

impl<TErr> fmt::Display for TorTransportError<TErr>
where
    TErr: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TorTransportError::Transport(err) => write!(f, "{}", err),
            TorTransportError::MultiaddrNotSupported(a) => {
                write!(f, "Unsupported resolved address: {}", a)
            }
            TorTransportError::SocksError(err) => write!(f, "{}", err),
        }
    }
}

impl<TErr> std::error::Error for TorTransportError<TErr> where TErr: fmt::Debug + fmt::Display {}

impl From<std::io::Error> for TorTransportError<std::io::Error> {
    fn from(error: std::io::Error) -> Self {
        TorTransportError::Transport(error)
    }
}

pub struct TorTransportWrapper<T> {
    inner: Arc<Mutex<T>>,
    proxy_addr: SocketAddr,
}

impl<T> TorTransportWrapper<T> {
    pub fn new(
        inner: T,
        proxy_addr: SocketAddr,
    ) -> Result<TorTransportWrapper<T>, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            proxy_addr,
        })
    }
}

fn to_onion3_domain<'a>(addr: Multiaddr) -> Option<(String, u16)> {
    let mut addr = addr.clone();
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

impl<T> Transport for TorTransportWrapper<T>
where
    T: Transport<Output = TcpStream> + Send + Unpin + 'static,
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
            .lock()
            .listen_on(addr)
            .map_err(|e| e.map(TorTransportError::Transport))
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.lock().remove_listener(id)
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let dest_addr = addr.clone();
        let proxy_addr = self.proxy_addr.clone();
        let inner = self.inner.clone();
        Ok(async move {
            if let Some((host, port)) = to_onion3_domain(dest_addr.clone()) {
                match Socks5Stream::connect(proxy_addr, (host, port)).await {
                    Ok(connection) => Ok(TcpStream(connection.into_inner())),
                    Err(error) => Err(TorTransportError::SocksError(error)),
                }
            } else {
                let dial = inner.lock().dial(dest_addr);
                match dial {
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
        let inner = self.inner.clone();
        Ok(async move {
            let dial = inner.lock().dial_as_listener(addr);
            match dial {
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
        self.inner.lock().address_translation(listen, observed)
    }

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        let mut inner = self.inner.lock();
        Transport::poll(Pin::new(inner.deref_mut()), cx).map(|event| {
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
