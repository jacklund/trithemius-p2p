use libp2p::{
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade},
    identity,
    mplex,
    noise,
    swarm::{NetworkBehaviour, Swarm, SwarmBuilder},
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    tcp::TokioTcpConfig,
    PeerId,
    Transport,
};

pub fn create_transport(id_keys: &identity::Keypair) -> Boxed<(PeerId, StreamMuxerBox)> {
    // Create a keypair for authenticated encryption of the transport.
    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(id_keys)
        .expect("Signing libp2p-noise static DH keypair failed.");

    TokioTcpConfig::new()
        .nodelay(true)
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed()
}

pub async fn create_swarm<T: NetworkBehaviour>(
    behaviour: T,
    transport: Boxed<(PeerId, StreamMuxerBox)>,
    peer_id: PeerId,
) -> Swarm<T> {
    SwarmBuilder::new(transport, behaviour, peer_id)
        // We want the connection background tasks to be spawned
        // onto the tokio runtime.
        .executor(Box::new(|fut| {
            tokio::spawn(fut);
        }))
        .build()
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
