use async_trait::async_trait;
use libp2p::{
    core::{connection::ListenerId, muxing::StreamMuxerBox, transport::Boxed, upgrade},
    futures::StreamExt,
    identity,
    mplex,
    noise,
    swarm::{
        ConnectionHandler, IntoConnectionHandler, NetworkBehaviour, Swarm, SwarmBuilder, SwarmEvent,
    },
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    tcp::TokioTcpConfig,
    Multiaddr,
    PeerId,
    Transport,
    TransportError,
};
use tokio::io;

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

#[async_trait]
pub trait InputHandler<B: NetworkBehaviour> {
    type Event;

    async fn get_input(&mut self) -> Option<io::Result<Self::Event>>;

    fn handle_input(
        &mut self,
        engine: &mut Engine<B>,
        line: Option<Self::Event>,
    ) -> Result<(), std::io::Error>;
}

pub trait EventHandler<B: NetworkBehaviour> {
    fn handle_event(
        &mut self,
        engine: &mut Engine<B>,
        event: SwarmEvent<B::OutEvent, <<<B as NetworkBehaviour>::ConnectionHandler as IntoConnectionHandler>::Handler as ConnectionHandler>::Error>,
    ) -> Result<(), std::io::Error>;
}

pub struct Engine<B: NetworkBehaviour> {
    swarm: Swarm<B>,
}

impl<B> Engine<B>
where
    B: NetworkBehaviour,
{
    pub async fn new(
        behaviour: B,
        transport: Boxed<(PeerId, StreamMuxerBox)>,
        peer_id: PeerId,
    ) -> Self {
        let swarm = SwarmBuilder::new(transport, behaviour, peer_id)
            // We want the connection background tasks to be spawned
            // onto the tokio runtime.
            .executor(Box::new(|fut| {
                tokio::spawn(fut);
            }))
            .build();

        Engine { swarm }
    }

    pub fn listen(
        &mut self,
        addr: Multiaddr,
    ) -> Result<ListenerId, TransportError<std::io::Error>> {
        self.swarm.listen_on(addr)
    }

    pub fn swarm(&mut self) -> &mut Swarm<B> {
        &mut self.swarm
    }

    pub async fn run<I: InputHandler<B>, E: EventHandler<B>>(
        &mut self,
        mut input_handler: I,
        mut event_handler: E,
    ) -> Result<(), std::io::Error> {
        loop {
            tokio::select! {
                line = input_handler.get_input() => {
                    input_handler.handle_input(self, line.transpose()?)?;
                },
                event = self.swarm().select_next_some() => {
                    event_handler.handle_event(self, event)?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
