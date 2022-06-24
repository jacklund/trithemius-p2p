pub mod engine_event;

use async_trait::async_trait;
use chrono::{DateTime, Local};
use engine_event::EngineEvent;
use futures::stream::FusedStream;
use libp2p::{
    core::{connection::ListenerId, muxing::StreamMuxerBox, transport::Boxed, upgrade},
    futures::StreamExt,
    gossipsub::{
        error::{PublishError, SubscriptionError},
        Gossipsub, GossipsubConfigBuilder, GossipsubMessage, IdentTopic, MessageAuthenticity,
        MessageId, ValidationMode,
    },
    identity,
    mplex,
    noise,
    ping::{Ping, PingConfig},
    swarm::{DialError, NetworkBehaviour, Swarm, SwarmBuilder},
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    tcp::TokioTcpConfig,
    Multiaddr,
    NetworkBehaviour,
    PeerId,
    Transport,
    TransportError,
};
use log::debug;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "EngineEvent")]
pub struct EngineBehaviour {
    pub_sub: Gossipsub,
    ping: Ping,
}

#[derive(Debug)]
pub enum InputEvent {
    Message { topic: IdentTopic, message: Vec<u8> },
    Shutdown,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub date: DateTime<Local>,
    pub user: PeerId,
    pub message: String,
}

impl ChatMessage {
    pub fn new(user: PeerId, message: String) -> ChatMessage {
        ChatMessage {
            date: Local::now(),
            user,
            message,
        }
    }
}

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
pub trait Handler<B: NetworkBehaviour, F: FusedStream> {
    type Event;

    fn handle_input(
        &mut self,
        engine: &mut Engine,
        line: F::Item,
    ) -> Result<Option<InputEvent>, std::io::Error>;

    fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: EngineEvent,
    ) -> Result<Option<EngineEvent>, std::io::Error>;

    fn update(&mut self) -> Result<(), std::io::Error>;
}

pub struct Engine {
    swarm: Swarm<EngineBehaviour>,
}

impl Engine {
    pub fn new(
        key: identity::Keypair,
        transport: Boxed<(PeerId, StreamMuxerBox)>,
        peer_id: PeerId,
    ) -> Result<Self, std::io::Error> {
        // To content-address message, we can take the hash of message and use it as an ID.
        let message_id_fn = |message: &GossipsubMessage| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            MessageId::from(s.finish().to_string())
        };

        // Set a custom gossipsub
        let gossipsub_config = GossipsubConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
            .validation_mode(ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message signing)
            .message_id_fn(message_id_fn) // content-address messages. No two messages of the same content will be propagated.
            .build()
            .expect("Valid config");

        let behaviour = EngineBehaviour {
            pub_sub: Gossipsub::new(MessageAuthenticity::Signed(key), gossipsub_config)
                .expect("Correct configuration"),
            ping: Ping::new(PingConfig::new().with_keep_alive(true)),
        };

        let swarm = SwarmBuilder::new(transport, behaviour, peer_id)
            // We want the connection background tasks to be spawned
            // onto the tokio runtime.
            .executor(Box::new(|fut| {
                tokio::spawn(fut);
            }))
            .build();

        Ok(Engine { swarm })
    }

    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), DialError> {
        self.swarm().dial(addr)
    }

    pub fn listen(
        &mut self,
        addr: Multiaddr,
    ) -> Result<ListenerId, TransportError<std::io::Error>> {
        self.swarm.listen_on(addr)
    }

    pub fn subscribe(&mut self, topic: &str) -> Result<bool, SubscriptionError> {
        self.swarm
            .behaviour_mut()
            .pub_sub
            .subscribe(&IdentTopic::new(topic))
    }

    pub fn publish(&mut self, topic: IdentTopic, data: Vec<u8>) -> Result<MessageId, PublishError> {
        debug!(
            "Publishing '{}' on topic {}",
            std::str::from_utf8(&data).unwrap(),
            topic
        );
        let ret = self.swarm.behaviour_mut().pub_sub.publish(topic, data);
        debug!("Returned from publish, ret = {:?}", ret);
        ret
    }

    pub fn swarm(&mut self) -> &mut Swarm<EngineBehaviour> {
        &mut self.swarm
    }

    pub async fn run<F: FusedStream + std::marker::Unpin, H: Handler<EngineBehaviour, F>>(
        &mut self,
        mut input_stream: F,
        mut handler: H,
    ) -> Result<(), std::io::Error> {
        loop {
            tokio::select! {
                line = input_stream.select_next_some() => {
                    match handler.handle_input(self, line)? {
                        Some(InputEvent::Message { topic, message }) => {
                            debug!("Got message");
                            let _message_id = self.publish(topic, message).unwrap();
                            debug!("Published message");
                        },
                        Some(InputEvent::Shutdown) => {
                            debug!("Got shutdown");
                            break Ok(());
                        }
                        _ => { },
                    }
                },
                event = self.swarm().select_next_some() => {
                    debug!("Got event {:?}", event);
                    handler.handle_event(self, event.into())?;
                }
            }

            handler.update()?;
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
