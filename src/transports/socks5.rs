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

pub struct Socks5Transport {
    proxy_addr: Multiaddr,
}

impl Socks5Transport {
    pub fn new(proxy_addr: Multiaddr) -> Self {
        Self { proxy_addr }
    }
}

impl Transport for Socks5Transport {
    type Output = TcpStream;
    type Error = tokio_socks::Error;
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
        let socket_addr = match multiaddr_to_socketaddr(addr.clone()) {
            Ok(socket_addr) => socket_addr,
            Err(_) => Err(TransportError::MultiaddrNotSupported(addr))?,
        };
        let proxy_addr = multiaddr_to_socketaddr(self.proxy_addr.clone())
            .map_err(|e| TransportError::Other(e))?;
        Ok(async move {
            match Socks5Stream::connect(proxy_addr, socket_addr).await {
                Ok(connection) => Ok(TcpStream(connection.into_inner())),
                Err(error) => Err(error),
            }
        }
        .boxed())
    }

    fn dial_as_listener(
        &mut self,
        _addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        // NOTE: Use https://docs.rs/tokio-socks/latest/tokio_socks/tcp/struct.Socks5Listener.html#method.bind
        unimplemented!()
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
