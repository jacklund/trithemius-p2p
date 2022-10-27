use crate::cli::{Cli, Discovery, NatTraversal};
use crate::engine_event::EngineEvent;
use crate::tor::{
    auth::TorAuthentication,
    control_connection::{OnionService, TorControlConnection},
    error::TorError,
};
use crate::transports::create_transport;
use crate::Handler;
use futures::stream::FusedStream;
use libp2p::{
    autonat::{self, Behaviour as Autonat, Config as AutonatConfig},
    core::transport::ListenerId,
    futures::StreamExt,
    gossipsub::{
        error::{PublishError, SubscriptionError},
        Gossipsub, GossipsubConfig, IdentTopic, MessageAuthenticity, MessageId,
    },
    identify::{Identify, IdentifyConfig},
    identity,
    kad::{record::store::MemoryStore, Kademlia, KademliaConfig, QueryId},
    mdns::{Mdns, MdnsConfig},
    ping::{Ping, PingConfig},
    rendezvous::{
        client::Behaviour as RendezvousClientBehaviour,
        server::{Behaviour as RendezvousServerBehaviour, Config as RendezvousServerConfig},
        Cookie, Namespace,
    },
    swarm::behaviour::toggle::Toggle,
    swarm::{DialError, NetworkBehaviour, Swarm, SwarmBuilder},
    Multiaddr, NetworkBehaviour, PeerId, TransportError,
};
use libp2p_dcutr::behaviour::Behaviour as Dcutr;
use log::debug;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use tokio::sync::mpsc;

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
    pub use_tor: bool,
    pub tor_proxy_address: Option<Multiaddr>,
    pub tor_control_address: Option<Multiaddr>,
    pub socks_proxy_address: Option<Multiaddr>,
}

impl From<Cli> for EngineConfig {
    fn from(cli: Cli) -> EngineConfig {
        let mut config = EngineConfig::default();
        if cli.discovery.is_some() {
            for discovery_type in cli.discovery.unwrap() {
                match discovery_type {
                    Discovery::Kademlia => {
                        config.kademlia_type = Some(KademliaType::Ip2p);
                        config.kademlia_config = KademliaConfig::default();
                    }
                    Discovery::Mdns => config.mdns_config = Some(MdnsConfig::default()),
                    Discovery::Rendezvous => config.rendezvous_client = true,
                }
            }
        }
        if cli.nat_traversal.is_some() {
            for nat_traversal in cli.nat_traversal.unwrap() {
                match nat_traversal {
                    NatTraversal::Autonat => config.autonat_config = Some(AutonatConfig::default()),
                    NatTraversal::CircuitRelay => config.use_circuit_relay = true,
                    NatTraversal::Dcutr => config.use_dcutr = true,
                }
            }
        }
        if cli.rendezvous_server {
            config.rendezvous_server = true;
        }

        config.use_tor = cli.use_tor;
        config.tor_proxy_address = cli.tor_proxy_address;
        config.tor_control_address = cli.tor_control_address;

        config
    }
}

pub struct Engine {
    swarm: Swarm<EngineBehaviour>,
    tor_connection: Option<TorControlConnection>,
    peer_id: PeerId,
    query_map: HashMap<QueryId, PeerId>,
    event_queue: VecDeque<EngineEvent>,
    discovery_types: Vec<Discovery>,
    nat_traversal_types: Vec<NatTraversal>,
    listen_address_map: HashMap<Multiaddr, Multiaddr>,
    onion_service_channel: mpsc::Sender<OnionService>,
}

impl Engine {
    pub async fn new(
        keypair: identity::Keypair,
        config: EngineConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let peer_id = PeerId::from(keypair.public());

        let (onion_service_channel, transport) = create_transport(
            config.tor_proxy_address,
            config.use_tor,
            config.socks_proxy_address,
            &keypair,
        )?;

        let mut discovery = Vec::new();
        let mut nat_traversal = Vec::new();

        // Set up the various sub-behaviours
        let autonat = match config.autonat_config {
            Some(config) => {
                nat_traversal.push(NatTraversal::Autonat);
                Some(Autonat::new(peer_id, config))
            }
            None => None,
        };

        let dcutr = match config.use_dcutr {
            true => {
                nat_traversal.push(NatTraversal::Dcutr);
                Some(Dcutr::new())
            }
            false => None,
        };

        let kademlia = if config.kademlia_type.is_some() {
            let mut kademlia = Kademlia::new(peer_id, MemoryStore::new(peer_id));
            let bootaddr: Multiaddr = "/dnsaddr/bootstrap.libp2p.io".parse().unwrap();
            for peer in &KAD_BOOTNODES {
                kademlia.add_address(&PeerId::from_str(peer)?, bootaddr.clone());
            }
            kademlia.bootstrap()?;
            discovery.push(Discovery::Kademlia);
            Some(kademlia)
        } else {
            None
        };

        let mdns = match config.mdns_config {
            Some(config) => {
                discovery.push(Discovery::Mdns);
                Some(Mdns::new(config).await?)
            }
            None => None,
        };

        let rendezvous_client = match config.rendezvous_client {
            true => {
                discovery.push(Discovery::Rendezvous);
                Some(RendezvousClientBehaviour::new(keypair.clone()))
            }
            false => None,
        };

        let rendezvous_server = match config.rendezvous_server {
            true => Some(RendezvousServerBehaviour::new(
                RendezvousServerConfig::default(),
            )),
            false => None,
        };

        // Create the main behaviour
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

        // Build the swarm
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
            discovery_types: discovery,
            nat_traversal_types: nat_traversal,
            listen_address_map: HashMap::new(),
            onion_service_channel,
        })
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn discovery_types(&self) -> Vec<Discovery> {
        self.discovery_types.clone()
    }

    pub fn nat_traversal_types(&self) -> Vec<NatTraversal> {
        self.nat_traversal_types.clone()
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
        listen_address: Multiaddr,
    ) -> Result<Multiaddr, Box<dyn std::error::Error>> {
        self.get_tor_connection()
            .await?
            .authenticate(TorAuthentication::Null)
            .await?;

        let onion_service = self
            .get_tor_connection()
            .await?
            .create_transient_onion_service(virt_port, listen_address.clone())
            .await?;

        self.onion_service_channel
            .send(onion_service.clone())
            .await?;

        self.listen(onion_service.listen_address.clone())?;

        match self.swarm.behaviour_mut().kademlia.as_mut() {
            Some(kademlia) => {
                self.query_map.insert(kademlia.bootstrap()?, self.peer_id);
            }
            None => (),
        }

        Ok(onion_service.address)
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
                            listener_id,
                            ref address,
                        } => {
                            match self.listen_address_map.get(address) {
                                Some(mapped_address) => {
                                    self.swarm
                                        .add_external_address(mapped_address.clone(), libp2p::swarm::AddressScore::Infinite);
                                    handler.handle_event(self, EngineEvent::NewListenAddr { listener_id, address: mapped_address.clone() }).await?;
                                },
                                None => {
                                    self.swarm
                                        .add_external_address(address.clone(), libp2p::swarm::AddressScore::Infinite);
                                    handler.handle_event(self, engine_event).await?;
                                },
                            };
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
