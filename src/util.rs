use libp2p::{multiaddr::Protocol, Multiaddr};

pub fn multiaddr_get<P>(addr: &Multiaddr, predicate: P) -> Option<Protocol>
where
    P: FnMut(&Protocol) -> bool,
{
    addr.iter().find(predicate)
}

pub fn multiaddr_split<P>(
    addr: &Multiaddr,
    predicate: P,
) -> Option<(Multiaddr, Protocol, Multiaddr)>
where
    P: FnMut(&(usize, Protocol)) -> bool,
{
    if let Some((i, proto)) = addr.iter().enumerate().find(predicate) {
        let suffix = addr.iter().skip(i + 1).collect::<Multiaddr>();
        let prefix = addr.iter().take(i).collect::<Multiaddr>();
        Some((prefix, proto, suffix))
    } else {
        None
    }
}

pub fn multiaddr_replace<'a, P>(
    addr: &'a Multiaddr,
    predicate: P,
    replacement: &'a Multiaddr,
) -> Option<Multiaddr>
where
    P: FnMut(&(usize, Protocol)) -> bool,
{
    multiaddr_split(addr, predicate).map(|(prefix, _, suffix)| {
        prefix
            .iter()
            .chain(replacement.iter().chain(suffix.iter()))
            .collect::<Multiaddr>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiaddr_get() {
        let addr: Multiaddr = "/ip4/10.11.12.13/tcp/8080/tor/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2".parse().unwrap();
        match multiaddr_get(&addr, |p| matches!(p, Protocol::P2p(_))) {
            Some(protocol) => {
                assert_eq!(
                    "/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2"
                        .parse::<Multiaddr>()
                        .unwrap()
                        .pop()
                        .unwrap(),
                    protocol
                );
            }
            None => assert!(false),
        }

        match multiaddr_get(&addr, |p| matches!(p, Protocol::Http)) {
            Some(_) => assert!(false),
            None => assert!(true),
        }
    }

    #[test]
    fn test_multiaddr_split() {
        let addr: Multiaddr = "/ip4/10.11.12.13/tcp/8080/tor/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2".parse().unwrap();
        match multiaddr_split(&addr, |(_, p)| matches!(p, Protocol::Tor)) {
            Some((prefix, protocol, suffix)) => {
                assert_eq!(Protocol::Tor, protocol);
                assert_eq!(
                    "/ip4/10.11.12.13/tcp/8080".parse::<Multiaddr>().unwrap(),
                    prefix
                );
                assert_eq!(
                    "/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2"
                        .parse::<Multiaddr>()
                        .unwrap(),
                    suffix
                );
            }
            None => assert!(false),
        }

        let addr: Multiaddr =
            "/ip4/10.11.12.13/tcp/8080/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2"
                .parse()
                .unwrap();
        match multiaddr_split(&addr, |(_, p)| matches!(p, Protocol::Tor)) {
            Some((_, _, _)) => assert!(false),
            None => assert!(true),
        }
    }

    #[test]
    fn test_multiaddr_replace() {
        let addr: Multiaddr = "/ip4/10.11.12.13/tcp/8080/tor/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2".parse().unwrap();
        match multiaddr_replace(&addr, |(_, p)| matches!(p, Protocol::Tor), &"/http".parse().unwrap()) {
            Some(addr) => assert_eq!("/ip4/10.11.12.13/tcp/8080/http/p2p/12D3KooWKMziqhPLYnRhjHQ7Hdr45m4HhGUT6dovhAQM49ffPNe2".parse::<Multiaddr>().unwrap(), addr),
            None => assert!(false),
        }

        match multiaddr_replace(
            &addr,
            |(_, p)| matches!(p, Protocol::Http),
            &"/tor".parse().unwrap(),
        ) {
            Some(_) => assert!(false),
            None => assert!(true),
        }
    }
}
