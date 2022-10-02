use crate::tor::error::TorError;
use futures::future::MapErr;
use libp2p::{multiaddr::Protocol, Multiaddr};
use libp2p_core::transport::{ListenerId, Transport, TransportError, TransportEvent};
use libp2p_dns::TokioDnsConfig;
use libp2p_tcp::{tokio::TcpStream, GenTcpTransport};
use std::net::Ipv4Addr;
use std::task::Poll;
use tokio::sync::mpsc;

pub struct TorTransport {
    proxy_addr: Option<Multiaddr>,
    address_tx: mpsc::Sender<(Multiaddr, Multiaddr)>,
}

// Dial:
// - /dns/www.google.com/https/tor -> /dns/www.google.com/socks5/ip4/127.0.0.1/tcp/9050
// - /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234 ->
//      /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234/socks5/ip4/127.0.0.1/tcp/9050
// Listen:
// - /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234 ->
//      /ip4/127.0.0.1/tcp/1234
impl TorTransport {
    pub fn new(
        proxy_addr: Option<Multiaddr>,
        address_tx: mpsc::Sender<(Multiaddr, Multiaddr)>,
    ) -> Self {
        Self {
            proxy_addr,
            address_tx,
        }
    }

    fn do_dial(
        &mut self,
        mut addr: Multiaddr,
    ) -> Result<<Self as Transport>::Dial, TransportError<<Self as Transport>::Error>> {
        if self.proxy_addr.is_some() {
            let proxy_addr = self.proxy_addr.clone().unwrap();
            match addr.clone().pop() {
                // /dns/www.google.com/https/tor -> /dns/www.google.com/socks5/ip4/127.0.0.1/tcp/9050
                Some(Protocol::Tor) => {
                    let last = addr.pop().unwrap();
                    match last {
                        Protocol::Tor => {
                            for protocol in proxy_addr.iter() {
                                addr.push(protocol);
                            }
                        }
                        _ => {
                            addr.push(last);
                        }
                    }
                    Err(TransportError::MultiaddrNotSupported(addr))
                }

                // /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234 ->
                //      /onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234/socks5/ip4/127.0.0.1/tcp/9050
                Some(Protocol::Onion3(address)) => {
                    addr.pop();
                    let onion_address = format!(
                        "{}.onion",
                        base32::encode(
                            base32::Alphabet::RFC4648 { padding: false },
                            address.hash()
                        )
                        .to_ascii_lowercase()
                    );
                    addr.push(Protocol::Dns(std::borrow::Cow::Borrowed(&onion_address)));
                    addr.push(Protocol::Tcp(address.port()));
                    addr.push(Protocol::Socks5(proxy_addr));
                    Err(TransportError::MultiaddrNotSupported(addr))
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
    type Dial =
        std::pin::Pin<Box<dyn futures::Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = MapErr<
        <TokioDnsConfig<GenTcpTransport<libp2p_tcp::tokio::Tcp>> as Transport>::ListenerUpgrade,
        fn(
            <TokioDnsConfig<GenTcpTransport<libp2p_tcp::tokio::Tcp>> as Transport>::Error,
        ) -> Self::Error,
    >;

    // NOTE: We can't bind/listen on the Tor SOCKS proxy, so we can't listen for /tor
    // addresses; however we do listen on a local port (or unix socket) for Onion3 addresses
    fn listen_on(&mut self, addr: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        let protocol = addr.clone().pop();
        let mut new_address = Multiaddr::empty();
        match protocol {
            Some(Protocol::Onion3(address)) => {
                new_address.push(Protocol::Ip4(Ipv4Addr::LOCALHOST));
                new_address.push(Protocol::Tcp(address.port()));
                self.address_tx
                    .clone()
                    .try_send((new_address.clone(), addr))
                    .unwrap();
            }
            Some(_) => {
                new_address = addr;
            }
            None => (),
        }
        Err(TransportError::MultiaddrNotSupported(new_address))
    }

    fn remove_listener(&mut self, _id: ListenerId) -> bool {
        false
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let protocol = addr.clone().pop();
        match protocol {
            Some(Protocol::Tor) => self.do_dial(addr),
            Some(Protocol::Onion3(_)) => self.do_dial(addr),
            Some(_) => Err(TransportError::MultiaddrNotSupported(addr)),
            None => Err(TransportError::MultiaddrNotSupported(addr)),
        }
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.dial(addr)
    }

    fn address_translation(&self, listen: &Multiaddr, _observed: &Multiaddr) -> Option<Multiaddr> {
        None
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

    fn do_test(
        transport: &mut TorTransport,
        addr: &str,
        expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let address: Multiaddr = addr.parse().unwrap();
        match transport.dial(address.clone()) {
            Ok(_) => assert!(false, "Transport should not return a future"),
            Err(TransportError::MultiaddrNotSupported(addr)) => {
                assert_eq!(expected.parse::<Multiaddr>().unwrap(), addr);
            }
            Err(error) => Err(error)?,
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_dial_noproxy() -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = TorTransport::default();
        do_test(
            &mut transport,
            "/ip4/10.11.12.13/tcp/80",
            "/ip4/10.11.12.13/tcp/80",
        )?;
        do_test(
            &mut transport,
            "/dns/www.google.com/tcp/80",
            "/dns/www.google.com/tcp/80",
        )?;
        do_test(
            &mut transport,
            "/ip4/10.11.12.13/tcp/80/tor",
            "/ip4/10.11.12.13/tcp/80/tor",
        )?;
        do_test(
            &mut transport,
            "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234",
            "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234",
        )?;

        Ok(())
    }

    #[tokio::test]
    async fn test_dial_proxy() -> Result<(), Box<dyn std::error::Error>> {
        let mut transport = TorTransport::default();
        let (_tx, rx) = mpsc::channel(10);
        transport.initialize("/ip4/127.0.0.1/tcp/9050".parse().unwrap(), rx);
        do_test(
            &mut transport,
            "/ip4/10.11.12.13/tcp/80",
            "/ip4/10.11.12.13/tcp/80",
        )?;
        do_test(
            &mut transport,
            "/dns/www.google.com/tcp/80",
            "/dns/www.google.com/tcp/80",
        )?;
        do_test(
            &mut transport,
            "/ip4/10.11.12.13/tcp/80/tor",
            "/ip4/10.11.12.13/tcp/80/ip4/127.0.0.1/tcp/9050",
        )?;
        do_test(
            &mut transport,
            "/onion3/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd:1234",
            "/dns/vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion/tcp/1234/socks5/ip4/127.0.0.1/tcp/9050",
        )?;

        Ok(())
    }
}
