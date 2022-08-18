pub mod engine_event;
pub mod subscriptions;
pub mod tor;

use crate::tor::{
    auth::TorAuthentication,
    control_connection::{OnionService, TorControlConnection},
    error::TorError,
    transport::TorTransportWrapper,
};
use async_trait::async_trait;
use chrono::{DateTime, Local};
use engine_event::EngineEvent;
use futures::stream::FusedStream;
use libp2p::{
    core::{muxing::StreamMuxerBox, transport::Boxed, transport::ListenerId, upgrade},
    futures::StreamExt,
    gossipsub::{
        error::{PublishError, SubscriptionError},
        Gossipsub, GossipsubConfigBuilder, IdentTopic, MessageAuthenticity, MessageId,
        ValidationMode,
    },
    identity,
    mplex,
    noise,
    ping::{Ping, PingConfig},
    swarm::{DialError, NetworkBehaviour, Swarm, SwarmBuilder},
    // `TokioTcpTransport` is available through the `tcp-tokio` feature.
    tcp::TokioTcpTransport,
    Multiaddr,
    NetworkBehaviour,
    PeerId,
    Transport,
    TransportError,
};
use libp2p_dns::TokioDnsConfig;
use libp2p_tcp::GenTcpConfig;
use log::debug;
use std::net::ToSocketAddrs;
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

// TODO: Add topic!!!
#[derive(Clone)]
pub struct ChatMessage {
    pub date: DateTime<Local>,
    pub topic: String,
    pub user: Option<PeerId>,
    pub message: String,
}

impl ChatMessage {
    pub fn new(user: Option<PeerId>, topic: String, message: String) -> ChatMessage {
        ChatMessage {
            date: Local::now(),
            topic,
            user,
            message,
        }
    }
}

pub fn create_transport(
    id_keys: &identity::Keypair,
) -> Result<Boxed<(PeerId, StreamMuxerBox)>, Box<dyn std::error::Error>> {
    // Create a keypair for authenticated encryption of the transport.
    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(id_keys)
        .expect("Signing libp2p-noise static DH keypair failed.");

    // We wrap the base libp2p tokio TCP transport inside the tokio DNS transport, inside our Tor
    // wrapper. Tor wrapper has to be first, so that an onion address isn't resolved by the DNS
    // layer. We need the DNS layer there in case we get a DNS hostname (unlikely, but possible).
    Ok(TorTransportWrapper::new(
        TokioDnsConfig::system(TokioTcpTransport::new(
            GenTcpConfig::default().nodelay(true),
        ))?,
        ("127.0.0.1", 9050) // TODO: Configure the TOR SOCKS5 proxy address
            .to_socket_addrs()?
            .next()
            .unwrap(),
    )?
    .upgrade(upgrade::Version::V1)
    .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
    .multiplex(mplex::MplexConfig::new())
    .boxed())
}

#[async_trait]
pub trait Handler<B: NetworkBehaviour, F: FusedStream> {
    type Event;

    async fn handle_input(
        &mut self,
        engine: &mut Engine,
        line: F::Item,
    ) -> Result<Option<InputEvent>, Box<dyn std::error::Error>>;

    async fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: EngineEvent,
    ) -> Result<Option<EngineEvent>, std::io::Error>;

    async fn handle_error(&mut self, error_message: &str);

    async fn update(&mut self) -> Result<(), std::io::Error>;
}

pub struct Engine {
    swarm: Swarm<EngineBehaviour>,
    tor_connection: Option<TorControlConnection>,
}

impl Engine {
    pub fn new(
        key: identity::Keypair,
        transport: Boxed<(PeerId, StreamMuxerBox)>,
        peer_id: PeerId,
    ) -> Result<Self, std::io::Error> {
        // Set a custom gossipsub
        let gossipsub_config = GossipsubConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
            .validation_mode(ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message signing)
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

        Ok(Engine {
            swarm,
            tor_connection: None,
        })
    }

    async fn get_tor_connection(&mut self) -> Result<&mut TorControlConnection, TorError> {
        if self.tor_connection.is_none() {
            self.tor_connection = Some(TorControlConnection::connect("127.0.0.1:9051").await?);
        }

        Ok(self.tor_connection.as_mut().unwrap())
    }

    pub async fn create_transient_onion_service(
        &mut self,
        virt_port: u16,
        target_port: u16,
    ) -> Result<OnionService, Box<dyn std::error::Error>> {
        self.get_tor_connection()
            .await?
            .authenticate(TorAuthentication::Null)
            .await?;

        let onion_service = self
            .get_tor_connection()
            .await?
            .create_transient_onion_service(virt_port, target_port)
            .await?;

        self.listen(format!("/ip4/127.0.0.1/tcp/{}", target_port).parse()?)?;

        Ok(onion_service)
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

    pub fn unsubscribe(&mut self, topic: &str) -> Result<bool, PublishError> {
        self.swarm
            .behaviour_mut()
            .pub_sub
            .unsubscribe(&IdentTopic::new(topic))
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            handler.update().await?;

            tokio::select! {
                line = input_stream.select_next_some() => {
                    match handler.handle_input(self, line).await? {
                        Some(InputEvent::Message { topic, message }) => {
                            match self.publish(topic, message) {
                                Ok(_message_id) => (),
                                Err(error) => {
                                    handler.handle_error(&format!("Error publishing message: {}", error)).await;
                                }
                            }
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
                    handler.handle_event(self, event.into()).await?;
                }
            }
        }
    }
}

// #[cfg(test)]
// mod tests {
//     #[test]
//     fn it_works() {
//         let result = 2 + 2;
//         assert_eq!(result, 4);
//     }
// }
