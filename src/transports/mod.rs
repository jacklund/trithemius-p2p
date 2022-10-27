pub mod socks5;
pub mod tor;

use crate::tor::control_connection::OnionService;
use crate::transports::{socks5::Socks5Transport, tor::TorTransport};
use libp2p::{
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade},
    identity, mplex, noise, Multiaddr, PeerId, Transport,
};
use libp2p_dns::TokioDnsConfig;
use libp2p_tcp::{GenTcpConfig, TokioTcpTransport};
use tokio::sync::mpsc;

pub fn create_transport(
    tor_proxy_address: Option<Multiaddr>,
    use_tor: bool,
    socks_proxy_address: Option<Multiaddr>,
    keypair: &identity::Keypair,
) -> Result<(mpsc::Sender<OnionService>, Boxed<(PeerId, StreamMuxerBox)>), std::io::Error> {
    let (onion_service_channel, rx) = mpsc::channel(1);
    let mut tor_transport = TorTransport::new().use_onion_service_channel(rx);
    if let Some(tor_proxy_address) = tor_proxy_address {
        tor_transport = tor_transport.use_proxy_address(tor_proxy_address);
    } else {
        tor_transport = tor_transport.use_default_proxy_address();
    }
    if use_tor {
        tor_transport = tor_transport.always_use();
    }

    let mut socks_transport = Socks5Transport::new();
    if let Some(socks_proxy_address) = socks_proxy_address {
        socks_transport = socks_transport.default_proxy_address(socks_proxy_address);
    }

    let dns_transport = TokioDnsConfig::system(TokioTcpTransport::new(
        GenTcpConfig::default().nodelay(true),
    ))?;

    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(keypair)
        .expect("Signing libp2p-noise static DH keypair failed.");

    let transport = tor_transport
        .or_transport(socks_transport)
        .or_transport(dns_transport)
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed();

    Ok((onion_service_channel, transport))
}
