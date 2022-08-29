use crate::ui::{Renderer, UI};
use async_trait::async_trait;
use clap::{Parser, ValueEnum};
use crossterm::event::{Event as TermEvent, EventStream};
use futures::task::Poll;
use futures_lite::stream::StreamExt;
use libp2p::{
    autonat::Config as AutonatConfig, core::ConnectedPoint, identity, kad::KademliaConfig,
    mdns::MdnsConfig, PeerId,
};
use log::debug;
// use log::LevelFilter;
// use simple_logging;
use std::pin::Pin;
use std::task::Context;
use trithemiuslib::{
    engine_event::EngineEvent, ChatMessage, Engine, EngineBehaviour, EngineConfig, Handler,
    InputEvent, KademliaType,
};

pub mod ui;

struct TermInputStream {
    reader: EventStream,
}

impl TermInputStream {
    fn new() -> Self {
        Self {
            reader: EventStream::new(),
        }
    }
}

impl futures::stream::Stream for TermInputStream {
    type Item = Result<TermEvent, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.reader.poll_next(cx)
    }
}

impl futures::stream::FusedStream for TermInputStream {
    fn is_terminated(&self) -> bool {
        false
    }
}

struct MyHandler {
    my_identity: PeerId,
    cli: Cli,
    ui: UI,
    known_peers: Vec<PeerId>,
    renderer: Renderer,
}

impl MyHandler {
    fn new(my_identity: PeerId, cli: Cli) -> Self {
        Self {
            my_identity,
            cli,
            ui: UI::new(my_identity),
            known_peers: Vec::new(),
            renderer: Renderer::new(),
        }
    }
}

#[async_trait]
impl Handler<EngineBehaviour, TermInputStream> for MyHandler {
    type Event = TermEvent;

    async fn startup(&mut self, engine: &mut Engine) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(address) = &self.cli.listen {
            self.ui.listen(engine, address)?;
        }

        if let Some(address) = &self.cli.connect {
            self.ui.connect(engine, address)?;
        }

        if let Some(topic) = &self.cli.subscribe {
            self.ui.subscribe(engine, topic);
        }

        if let Some(port) = &self.cli.create_onion_service {
            self.ui.create_onion_service(engine, &port, None).await;
        }

        Ok(())
    }

    async fn handle_input(
        &mut self,
        engine: &mut Engine,
        event: Result<Self::Event, std::io::Error>,
    ) -> Result<Option<InputEvent>, Box<dyn std::error::Error>> {
        let event = self.ui.handle_input_event(engine, event?).await?;
        // debug!("handle_input, got event {:?}", event);

        Ok(event)
    }

    async fn handle_event(
        &mut self,
        _engine: &mut Engine,
        event: EngineEvent,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        // debug!("Event: {:?}", event);
        Ok(match event {
            EngineEvent::MdnsExpired(list) => {
                for peer in list {
                    debug!("Peer {} left", peer.peer_id);
                }
                None
            }
            EngineEvent::Message {
                source,
                propagation_source: _,
                topic,
                message_id: _,
                sequence_number: _,
                message,
            } => {
                self.ui
                    .add_message(ChatMessage::new(source, topic.to_string(), message));
                None
            }
            EngineEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established: _,
                concurrent_dial_errors: _,
            } => {
                let address = match endpoint.clone() {
                    ConnectedPoint::Dialer {
                        address,
                        role_override: _,
                    } => address,
                    ConnectedPoint::Listener {
                        local_addr: _,
                        send_back_addr,
                    } => send_back_addr,
                };
                if self.ui.connecting_to(&address) {
                    self.ui
                        .log_info(&format!("Connected to {} at {:?}", peer_id, endpoint,));
                }
                None
            }
            EngineEvent::ConnectionClosed {
                peer_id,
                endpoint,
                num_established: _,
                cause,
            } => {
                let address = match endpoint.clone() {
                    ConnectedPoint::Dialer {
                        address,
                        role_override: _,
                    } => address,
                    ConnectedPoint::Listener {
                        local_addr: _,
                        send_back_addr,
                    } => send_back_addr,
                };
                if self.ui.connecting_to(&address) {
                    self.ui.log_info(&format!(
                        "Connection closed to {} at {:?}: {:?}",
                        peer_id, endpoint, cause
                    ));
                }
                self.ui.disconnected_from(&address);
                None
            }
            EngineEvent::IncomingConnection {
                local_addr,
                send_back_addr,
            } => {
                if send_back_addr.is_empty() {
                    self.ui
                        .log_info(&format!("Incoming connection to {}", local_addr,));
                } else {
                    self.ui.log_info(&format!(
                        "Incoming connection to {} from {}",
                        local_addr, send_back_addr,
                    ));
                }
                None
            }
            EngineEvent::NewListenAddr {
                listener_id,
                address,
            } => {
                self.ui.log_info(&format!(
                    "Listener {:?} listening on {}",
                    listener_id, address,
                ));
                None
            }
            EngineEvent::Dialing(_peer_id) => {
                // self.ui.log_info(&format!("Dialing {}", peer_id,));
                None
            }
            EngineEvent::MdnsDiscovered(peers) => {
                self.ui.log_info(&format!(
                    "Discovered peers: {}",
                    peers
                        .iter()
                        .map(|p| format!("{}@{}", p.peer_id, p.address))
                        .collect::<Vec<String>>()
                        .join(", ")
                ));
                None
            }
            EngineEvent::GossipSubscribed { peer_id, topic } => {
                if self.ui.subscribed(topic.as_str()) {
                    self.ui
                        .log_info(&format!("Peer {} subscribed to {}", peer_id, topic));
                }
                None
            }
            EngineEvent::GossipUnsubscribed { peer_id, topic } => {
                if self.ui.subscribed(topic.as_str()) {
                    self.ui
                        .log_info(&format!("Peer {} unsubscribed to {}", peer_id, topic));
                }
                None
            }
            _ => None,
        })
    }

    async fn handle_error(&mut self, error_message: &str) {
        self.ui.log_error(error_message);
    }

    async fn update(&mut self) -> Result<(), std::io::Error> {
        // debug!("Called handler::update()");
        self.renderer.render(&self.ui)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Discovery {
    Kademlia,
    Mdns,
    Rendezvous,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum NatTraversal {
    Autonat,
    CircuitRelay,
    Dcutr,
}

#[derive(Clone, Parser)]
#[clap(author, version, about, long_about = None)]
pub struct Cli {
    #[clap(long, value_parser, multiple_values = true, use_value_delimiter = true)]
    discovery: Option<Vec<Discovery>>,

    #[clap(long, value_parser, multiple_values = true, use_value_delimiter = true)]
    nat_traversal: Option<Vec<NatTraversal>>,

    #[clap(long, value_parser, value_name = "TOPIC")]
    subscribe: Option<String>,

    #[clap(long, value_parser, value_name = "ADDRESS")]
    listen: Option<String>,

    #[clap(long, value_parser, value_name = "ADDRESS")]
    connect: Option<String>,

    #[clap(long, value_name = "ADDRESS")]
    create_onion_service: Option<String>,
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
                    Discovery::Rendezvous => config.use_rendezvous = true,
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
        config
    }
}

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // env_logger::init();
    // simple_logging::log_to_file("trithemius.log", LevelFilter::Debug)?;

    let cli = Cli::parse();

    // Create a random Keypair
    let keypair = identity::Keypair::generate_ed25519();

    // Create an engine to manage peers and events.
    let engine_config: EngineConfig = cli.clone().into();
    let mut engine = Engine::new(keypair, engine_config).await?;

    let mut handler = MyHandler::new(engine.peer_id(), cli);
    handler
        .ui
        .log_info(&format!("Local peer id: {:?}", engine.peer_id()));

    engine.run(TermInputStream::new(), handler).await
}
