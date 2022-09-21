use crate::tor::error::TorError;
use crate::transports::socks5::multiaddr_to_socketaddr;
use futures::future::MapErr;
use futures::FutureExt;
use libp2p::{multiaddr::Protocol, Multiaddr};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_dns::TokioDnsConfig;
use libp2p_tcp::{tokio::TcpStream, GenTcpTransport};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::task::Poll;
use tokio_socks::tcp::Socks5Stream;

pub struct TorTransport {
    proxy_addr: Multiaddr,
    tor_map: HashMap<Multiaddr, Multiaddr>,
}

impl TorTransport {
    pub fn new(proxy_addr: Multiaddr) -> Result<Self, std::io::Error> {
        Ok(Self {
            proxy_addr,
            tor_map: HashMap::new(),
        })
    }

    fn do_dial(
        &mut self,
        addr: Multiaddr,
    ) -> Result<<Self as Transport>::Dial, TransportError<<Self as Transport>::Error>> {
        let socket_addr = match multiaddr_to_socketaddr(addr.clone()) {
            Ok(socket_addr) => socket_addr,
            Err(_) => Err(TransportError::MultiaddrNotSupported(addr))?,
        };
        let proxy_addr = multiaddr_to_socketaddr(self.proxy_addr.clone())
            .map_err(|e| TransportError::Other(e.into()))?;
        Ok(async move {
            match Socks5Stream::connect(proxy_addr, socket_addr).await {
                Ok(connection) => Ok(TcpStream(connection.into_inner())),
                Err(error) => Err(error.into()),
            }
        }
        .boxed())
    }
}

impl Transport for TorTransport {
    type Output = TcpStream;
    type Error = TorError;
    type Dial =
        std::pin::Pin<Box<dyn futures::Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = MapErr<
        <TokioDnsConfig<GenTcpTransport<libp2p_tcp::tokio::Tcp>> as Transport>::ListenerUpgrade,
        fn(
            <TokioDnsConfig<GenTcpTransport<libp2p_tcp::tokio::Tcp>> as Transport>::Error,
        ) -> Self::Error,
    >;

    fn listen_on(&mut self, addr: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        let protocol = addr.clone().pop();
        let mut new_address = Multiaddr::empty();
        match protocol {
            Some(Protocol::Onion3(address)) => {
                new_address.push(Protocol::Tcp(address.port()));
                new_address.push(Protocol::Ip4(Ipv4Addr::LOCALHOST));
                self.tor_map.insert(addr, new_address.clone());
            }
            Some(_) => {
                new_address = addr.clone();
            }
            None => (),
        }
        Err(TransportError::MultiaddrNotSupported(new_address))
    }

    fn remove_listener(&mut self, _id: ListenerId) -> bool {
        false
    }

    fn dial(&mut self, mut addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let protocol = addr.pop();
        match protocol {
            Some(Protocol::Tor) => self.do_dial(addr),
            Some(Protocol::Onion3(address)) => {
                let onion_address = format!(
                    "{}.onion",
                    base32::encode(base32::Alphabet::RFC4648 { padding: false }, address.hash())
                );
                addr.push(Protocol::Tcp(address.port()));
                addr.push(Protocol::Dns(std::borrow::Cow::Borrowed(&onion_address)));
                self.do_dial(addr)
            }
            Some(protocol) => {
                addr.push(protocol);
                Err(TransportError::MultiaddrNotSupported(addr))
            }
            None => Err(TransportError::MultiaddrNotSupported(addr)),
        }
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        // TODO: Figure out what this actually means
        self.do_dial(addr)
    }

    fn address_translation(&self, listen: &Multiaddr, _observed: &Multiaddr) -> Option<Multiaddr> {
        self.tor_map.get(listen).map(|a| a.clone())
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
    use futures;
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

        let mut transport = TorTransport::new("/ip4/127.0.0.1/tcp/5000".parse().unwrap())?;

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
