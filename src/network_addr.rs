use base32;
use libp2p::{
    multiaddr::{Error, Onion3Addr, Protocol},
    Multiaddr,
};
use std::net::{Ipv4Addr, Ipv6Addr};

pub struct NetworkAddress {
    addr: Multiaddr,
}

impl NetworkAddress {
    pub fn as_multaddr_string(&self) -> String {
        self.addr.to_string()
    }

    pub fn as_network_string(&self) -> Result<String, Error> {
        let mut ip_or_dns = "<unknown>".to_string();
        let mut port = 80;
        for protocol in self.addr.iter() {
            match protocol {
                Protocol::Ip4(ipv4addr) => ip_or_dns = ipv4addr.to_string(),
                Protocol::Ip6(ipv6addr) => ip_or_dns = ipv6addr.to_string(),
                Protocol::Tcp(p) => port = p,
                Protocol::Udp(p) => port = p,
                Protocol::Dns(dns) => ip_or_dns = dns.to_string(),
                Protocol::Onion3(addr) => {
                    ip_or_dns = format!(
                        "{}.onion",
                        base32::encode(base32::Alphabet::RFC4648 { padding: false }, addr.hash())
                    )
                }
                _ => Err(Error::ParsingError(
                    format!("Unable to parse protocol {} as network string", protocol).into(),
                ))?,
            }
        }
        Ok(format!("{}:{}", ip_or_dns, port))
    }
}

impl From<Multiaddr> for NetworkAddress {
    fn from(addr: Multiaddr) -> Self {
        Self { addr }
    }
}

impl From<NetworkAddress> for Multiaddr {
    fn from(addr: NetworkAddress) -> Self {
        addr.addr
    }
}

impl std::str::FromStr for NetworkAddress {
    type Err = Error;

    fn from_str(addr: &str) -> Result<Self, Self::Err> {
        if addr.starts_with('/') {
            Ok(NetworkAddress {
                addr: Multiaddr::from_str(addr)?,
            })
        } else {
            let mut multiaddr = Multiaddr::empty();
            let parts = addr.split(':').collect::<Vec<&str>>();
            let port = match parts[1].parse::<u16>() {
                Ok(port) => port,
                Err(error) => Err(error)?,
            };
            let mut used_port = false;
            multiaddr.push(if let Ok(ipv4) = Ipv4Addr::from_str(parts[0]) {
                Protocol::Ip4(ipv4)
            } else if let Ok(ipv6) = Ipv6Addr::from_str(parts[0]) {
                Protocol::Ip6(ipv6)
            } else {
                if parts[0].ends_with(".onion") {
                    let hash_string = parts[0].split(".").collect::<Vec<&str>>()[0];
                    let hash: [u8; 35] =
                        base32::decode(base32::Alphabet::RFC4648 { padding: false }, hash_string)
                            .unwrap()
                            .as_slice()
                            .try_into()
                            .unwrap();
                    used_port = true;
                    Protocol::Onion3(Onion3Addr::from((hash, port)))
                } else {
                    Protocol::Dns(parts[0].into())
                }
            });
            if !used_port {
                multiaddr.push(Protocol::Tcp(port));
            }
            Ok(NetworkAddress { addr: multiaddr })
        }
    }
}
