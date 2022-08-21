use base32;
use libp2p::{
    multiaddr::{Error, Onion3Addr, Protocol},
    Multiaddr,
};
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkAddress {
    addr: Multiaddr,
}

impl std::fmt::Display for NetworkAddress {
    // This trait requires `fmt` with this exact signature.
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.addr)
    }
}

impl NetworkAddress {
    pub fn as_multaddr_string(&self) -> String {
        self.addr.to_string()
    }

    pub fn as_network_string(&self) -> Result<String, Error> {
        let mut ip_or_dns = "<unknown>".to_string();
        let mut port: Option<u16> = None;
        for protocol in self.addr.iter() {
            match protocol {
                Protocol::Ip4(ipv4addr) => ip_or_dns = ipv4addr.to_string(),
                Protocol::Ip6(ipv6addr) => ip_or_dns = ipv6addr.to_string(),
                Protocol::Tcp(p) => port = Some(p),
                Protocol::Udp(p) => port = Some(p),
                Protocol::Dns(dns) => ip_or_dns = dns.to_string(),
                Protocol::Onion3(addr) => {
                    ip_or_dns = format!(
                        "{}.onion",
                        base32::encode(base32::Alphabet::RFC4648 { padding: false }, addr.hash())
                            .to_lowercase()
                    );
                    port = Some(addr.port());
                }
                _ => Err(Error::ParsingError(
                    format!("Unable to parse protocol {} as network string", protocol).into(),
                ))?,
            }
        }

        if port.is_none() {
            Err(Error::ParsingError("No port specified".into()))?;
        }

        Ok(format!("{}:{}", ip_or_dns, port.unwrap()))
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

impl PartialEq<Multiaddr> for NetworkAddress {
    fn eq(&self, other: &Multiaddr) -> bool {
        self.addr == *other
    }
}

impl PartialEq<NetworkAddress> for Multiaddr {
    fn eq(&self, other: &NetworkAddress) -> bool {
        *self == other.addr
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
            if !addr.contains(':') {
                return Err(Error::ParsingError("No port specified".into()));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::Multiaddr;

    #[test]
    fn test_from_ip_port() -> Result<(), Box<dyn std::error::Error>> {
        let address_string = "1.2.3.4:22";
        let multiaddr_string = "/ip4/1.2.3.4/tcp/22";
        let network_address: NetworkAddress = address_string.parse()?;
        let multiaddr: Multiaddr = multiaddr_string.parse()?;

        assert_eq!(multiaddr, network_address);

        assert_eq!(address_string, network_address.as_network_string()?);
        assert_eq!(multiaddr_string, network_address.as_multaddr_string());

        Ok(())
    }

    #[test]
    fn test_from_dns_port() -> Result<(), Box<dyn std::error::Error>> {
        let address_string = "www.foo.com:22";
        let multiaddr_string = "/dns/www.foo.com/tcp/22";
        let network_address: NetworkAddress = address_string.parse()?;
        let multiaddr: Multiaddr = multiaddr_string.parse()?;

        assert_eq!(multiaddr, network_address);

        assert_eq!(address_string, network_address.as_network_string()?);
        assert_eq!(multiaddr_string, network_address.as_multaddr_string());

        Ok(())
    }

    #[test]
    fn test_from_multiaddr() -> Result<(), Box<dyn std::error::Error>> {
        let address_string = "1.2.3.4:22";
        let multiaddr_string = "/ip4/1.2.3.4/tcp/22";
        let network_address: NetworkAddress = multiaddr_string.parse()?;
        let multiaddr: Multiaddr = multiaddr_string.parse()?;

        assert_eq!(multiaddr, network_address);

        assert_eq!(address_string, network_address.as_network_string()?);
        assert_eq!(multiaddr_string, network_address.as_multaddr_string());

        Ok(())
    }

    #[test]
    fn test_from_onion_address() -> Result<(), Box<dyn std::error::Error>> {
        let address_string = "nytimesn7cgmftshazwhfgzm37qxb44r64ytbb2dj3x62d2lljsciiyd.onion:22";
        let multiaddr_string =
            "/onion3/nytimesn7cgmftshazwhfgzm37qxb44r64ytbb2dj3x62d2lljsciiyd:22";
        let network_address: NetworkAddress = address_string.parse()?;
        let multiaddr: Multiaddr = multiaddr_string.parse()?;

        assert_eq!(multiaddr, network_address);

        assert_eq!(address_string, network_address.as_network_string()?);
        assert_eq!(multiaddr_string, network_address.as_multaddr_string());

        Ok(())
    }
}
