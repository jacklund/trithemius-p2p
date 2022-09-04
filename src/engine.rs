use crate::engine_event::EngineEvent;
use crate::tor::{
    auth::TorAuthentication,
    control_connection::{OnionService, TorControlConnection},
    error::TorError,
    transport::TorDnsTransport,
};
use crate::Handler;
use futures::stream::FusedStream;
use libp2p::{
    autonat::{self, Behaviour as Autonat},
    core::{muxing::StreamMuxerBox, transport::Boxed, transport::ListenerId, upgrade},
    futures::StreamExt,
    gossipsub::{
        error::{PublishError, SubscriptionError},
        Gossipsub, GossipsubConfig, IdentTopic, MessageAuthenticity, MessageId,
    },
    identify::{Identify, IdentifyConfig},
    identity,
    kad::{record::store::MemoryStore, Kademlia, KademliaConfig, QueryId},
    mdns::{Mdns, MdnsConfig},
    mplex, noise,
    ping::{Ping, PingConfig},
    rendezvous::{
        client::Behaviour as RendezvousClientBehaviour,
        server::{Behaviour as RendezvousServerBehaviour, Config as RendezvousServerConfig},
        Cookie, Namespace,
    },
    swarm::behaviour::toggle::Toggle,
    swarm::{DialError, NetworkBehaviour, Swarm, SwarmBuilder},
    Multiaddr, NetworkBehaviour, PeerId, Transport, TransportError,
};
use libp2p_dcutr::behaviour::Behaviour as Dcutr;
use log::debug;
use std::collections::{HashMap, VecDeque};
use std::net::ToSocketAddrs;
use std::str::FromStr;

#[derive(Debug)]
pub enum EngineError {
    Error(String),
}

impl std::error::Error for EngineError {}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            EngineError::Error(error) => write!(f, "{}", error),
        }
    }
}

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "EngineEvent")]
pub struct EngineBehaviour {
    autonat: Toggle<Autonat>,
    dcutr: Toggle<Dcutr>,
    identify: Identify,
    kademlia: Toggle<Kademlia<MemoryStore>>,
    mdns: Toggle<Mdns>,
    ping: Ping,
    pub_sub: Gossipsub,
    rendezvous_client: Toggle<RendezvousClientBehaviour>,
    rendezvous_server: Toggle<RendezvousServerBehaviour>,
}

pub fn create_transport(
    keypair: &identity::Keypair,
) -> Result<Boxed<(PeerId, StreamMuxerBox)>, Box<dyn std::error::Error>> {
    // Create a keypair for authenticated encryption of the transport.
    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(keypair)
        .expect("Signing libp2p-noise static DH keypair failed.");

    // We wrap the base libp2p tokio TCP transport inside the tokio DNS transport, inside our Tor
    // wrapper. Tor wrapper has to be first, so that an onion address isn't resolved by the DNS
    // layer. We need the DNS layer there in case we get a DNS hostname (unlikely, but possible).
    Ok(TorDnsTransport::new(
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

const KAD_BOOTNODES: [&str; 4] = [
    "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
];

#[derive(Clone, Debug)]
pub enum KademliaType {
    Ip2p,
}

#[derive(Debug)]
pub enum InputEvent {
    Message { topic: IdentTopic, message: Vec<u8> },
    Shutdown,
}

#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    pub gossipsub_config: GossipsubConfig,
    pub autonat_config: Option<autonat::Config>,
    pub use_circuit_relay: bool,
    pub use_dcutr: bool,
    pub kademlia_type: Option<KademliaType>,
    pub kademlia_config: KademliaConfig,
    pub mdns_config: Option<MdnsConfig>,
    pub rendezvous_server: bool,
    pub rendezvous_client: bool,
}

pub struct Engine {
    swarm: Swarm<EngineBehaviour>,
    tor_connection: Option<TorControlConnection>,
    peer_id: PeerId,
    query_map: HashMap<QueryId, PeerId>,
    event_queue: VecDeque<EngineEvent>,
}

impl Engine {
    pub async fn new(
        keypair: identity::Keypair,
        config: EngineConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let peer_id = PeerId::from(keypair.public());
        let transport = create_transport(&keypair)?;

        let autonat = config
            .autonat_config
            .map(|config| Autonat::new(peer_id, config));

        let dcutr = match config.use_dcutr {
            true => Some(Dcutr::new()),
            false => None,
        };

        let kademlia = if config.kademlia_type.is_some() {
            let mut kademlia = Kademlia::new(peer_id, MemoryStore::new(peer_id));
            let bootaddr: Multiaddr = "/dnsaddr/bootstrap.libp2p.io".parse().unwrap();
            for peer in &KAD_BOOTNODES {
                kademlia.add_address(&PeerId::from_str(peer)?, bootaddr.clone());
            }
            kademlia.bootstrap()?;
            Some(kademlia)
        } else {
            None
        };

        let mdns = match config.mdns_config {
            Some(config) => Some(Mdns::new(config).await?),
            None => None,
        };

        let rendezvous_client = match config.rendezvous_client {
            true => Some(RendezvousClientBehaviour::new(keypair.clone())),
            false => None,
        };

        let rendezvous_server = match config.rendezvous_server {
            true => Some(RendezvousServerBehaviour::new(
                RendezvousServerConfig::default(),
            )),
            false => None,
        };

        let behaviour = EngineBehaviour {
            autonat: Toggle::from(autonat),
            dcutr: Toggle::from(dcutr),
            identify: Identify::new(IdentifyConfig::new(
                "ipfs/id/1.0.0".to_string(),
                keypair.public(),
            )),
            kademlia: Toggle::from(kademlia),
            mdns: Toggle::from(mdns),
            ping: Ping::new(PingConfig::new().with_keep_alive(true)),
            pub_sub: Gossipsub::new(
                MessageAuthenticity::Signed(keypair.clone()),
                config.gossipsub_config,
            )
            .expect("Correct configuration"),
            rendezvous_client: Toggle::from(rendezvous_client),
            rendezvous_server: Toggle::from(rendezvous_server),
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
            peer_id,
            query_map: HashMap::new(),
            event_queue: VecDeque::new(),
        })
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
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

        // Send the onion address down, the Tor transport layer will translate it
        self.listen(onion_service.address.clone())?;

        Ok(onion_service)
    }

    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), DialError> {
        self.swarm.dial(addr)
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

    pub fn register(
        &mut self,
        namespace: Namespace,
        rendezvous_node: PeerId,
    ) -> Result<(), EngineError> {
        match self.swarm.behaviour_mut().rendezvous_client.as_mut() {
            Some(client) => {
                client.register(namespace, rendezvous_node, None);
                Ok(())
            }
            None => Err(EngineError::Error(
                "Cannot register without rendezvous discovery enabled".to_string(),
            )),
        }
    }

    pub fn discover(
        &mut self,
        namespace: Option<Namespace>,
        cookie: Option<Cookie>,
        limit: Option<u64>,
        rendezvous_node: PeerId,
    ) -> Result<(), EngineError> {
        match self.swarm.behaviour_mut().rendezvous_client.as_mut() {
            Some(client) => {
                client.discover(namespace, cookie, limit, rendezvous_node);
                Ok(())
            }
            None => Err(EngineError::Error(
                "Cannot discover without rendezvous discovery enabled".to_string(),
            )),
        }
    }

    // This is kind of strange. If we're using Kademlia, we can't just ask
    // for the peer address, we need to first run a query for it. See
    // https://github.com/libp2p/rust-libp2p/issues/1568.
    pub async fn find_peer(&mut self, peer: &PeerId) {
        match self.swarm.behaviour_mut().kademlia.as_mut() {
            Some(kademlia) => {
                self.event_queue.push_back(
                    EngineEvent::SystemMessage(
                        format!("Initiating finding peer {} with Kademlia, please be patient, this might take a minute", peer)
                    )
                );
                let query_id = kademlia.get_closest_peers(*peer);

                // Insert query id and peer id into map, to be looked up
                // later when the query response comes in
                self.query_map.insert(query_id, *peer);
            }
            None => {
                let addresses = self.swarm.behaviour_mut().addresses_of_peer(peer);

                // Inject the event "artificially" into the event handler
                self.event_queue.push_back(EngineEvent::PeerAddresses {
                    peer: *peer,
                    addresses,
                });
            }
        }
    }

    pub async fn run<F: FusedStream + std::marker::Unpin, H: Handler<EngineBehaviour, F>>(
        &mut self,
        mut input_stream: F,
        mut handler: H,
    ) -> Result<(), Box<dyn std::error::Error>> {
        handler.startup(self).await?;

        loop {
            handler.update().await?;

            tokio::select! {
                event = input_stream.select_next_some() => {
                    match handler.handle_input(self, event).await? {
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
                event = self.swarm.select_next_some() => {
                    debug!("Got event {:?}", event);
                    let engine_event = event.into();
                    match engine_event {
                        EngineEvent::NewListenAddr {
                            listener_id: _,
                            ref address,
                        } => {
                            self.swarm
                                .add_external_address(address.clone(), libp2p::swarm::AddressScore::Infinite);
                            handler.handle_event(self, engine_event).await?;
                        },
                        // Kademlia query has completed, we can check the peer addresses now
                        EngineEvent::KadOutboundQueryCompleted {
                            id,
                            result: _,
                            stats: _,
                        } => {
                            if let Some(peer) = self.query_map.remove(&id) {
                                let addresses = self.swarm.behaviour_mut().addresses_of_peer(&peer);
                                handler.handle_event(self, EngineEvent::PeerAddresses {
                                    peer,
                                    addresses,
                                }).await?;
                            }
                            handler.handle_event(self, engine_event).await?;
                        }
                        _ => { handler.handle_event(self, engine_event).await?; }
                    }
                }
            }

            while let Some(event) = self.event_queue.pop_front() {
                handler.handle_event(self, event).await?;
            }
        }
    }
}
