use crate::ui::{Renderer, UI};
use async_trait::async_trait;
use clap::Parser;
use crossterm::event::{Event as TermEvent, EventStream};
use futures::task::Poll;
use futures_lite::stream::StreamExt;
use libp2p::{core::ConnectedPoint, identity, PeerId};
use log::debug;
use log::LevelFilter;
use simple_logging;
use std::pin::Pin;
use std::task::Context;
use trithemiuslib::{
    cli::{Cli, Discovery},
    engine_event::EngineEvent,
    ChatMessage, Engine, EngineBehaviour, EngineConfig, Handler, InputEvent,
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
        self.ui.startup(
            &mut engine.discovery_types(),
            &mut engine.nat_traversal_types(),
        );

        if let Some(address) = &self.cli.listen {
            self.ui.listen(engine, address)?;
        }

        if let Some(address) = &self.cli.connect {
            self.ui.connect(engine, address)?;
        }

        if let Some(topic) = &self.cli.subscribe {
            self.ui.subscribe(engine, topic);
        }

        if let Some(values) = &self.cli.create_onion_service {
            let listen_address = if values.len() > 1 {
                Some(values[1].as_str())
            } else {
                None
            };
            self.ui
                .create_onion_service(engine, &values[0], listen_address)
                .await;
        }

        if let Some(namespace_nodes) = &self.cli.register {
            for namespace_node in namespace_nodes {
                engine.register(namespace_node.clone().namespace, namespace_node.node_id)?;
            }
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
            EngineEvent::SystemMessage(message) => {
                self.ui.log_info(&message);
                None
            }
            EngineEvent::PeerAddresses { peer, addresses } => {
                if addresses.is_empty() {
                    self.ui
                        .log_info(&format!("Found no addresses for PeerId {}", peer));
                } else {
                    self.ui.log_info(&format!(
                        "Found the following addresses for PeerId {}:",
                        peer
                    ));
                    for address in addresses {
                        self.ui.log_info(&format!("   {}", address));
                    }
                }
                None
            }
            EngineEvent::OnionServiceReady(onion_service) => {
                self.ui.log_info(&format!(
                    "Onion service {} is ready to receive connections",
                    onion_service.address
                ));
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
            EngineEvent::Dialing(peer_id) => {
                self.ui.log_info(&format!("Dialing {}", peer_id,));
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
            EngineEvent::RendezvousServerPeerRegistered { peer, registration } => {
                self.ui.log_info(&format!(
                    "Peer {} registered with namespace {}",
                    peer, registration.namespace
                ));
                None
            }
            EngineEvent::RendezvousServerPeerNotRegistered {
                peer,
                namespace: _,
                error,
            } => {
                self.ui
                    .log_info(&format!("Peer {} not registered: {:?}", peer, error));
                None
            }
            EngineEvent::RendezvousServerPeerUnregistered { peer, namespace } => {
                self.ui.log_info(&format!(
                    "Peer {} unregistered, namespace {}",
                    peer, namespace
                ));
                None
            }
            EngineEvent::RendezvousClientRegistered {
                rendezvous_node,
                ttl,
                namespace,
            } => {
                self.ui.log_info(&format!(
                    "Registered with node {} in namespace {} with ttl {}",
                    rendezvous_node, namespace, ttl
                ));
                None
            }
            EngineEvent::RendezvousClientRegisterFailed(error) => {
                self.ui
                    .log_error(&format!("Registration failed: {}", error));
                None
            }
            EngineEvent::RendezvousClientDiscovered {
                rendezvous_node,
                registrations: _,
                cookie: _,
            } => {
                self.ui.log_info(&format!(
                    "Discovery succeeded for rendezvous node {}",
                    rendezvous_node
                ));
                None
            }
            EngineEvent::RendezvousClientDiscoverFailed {
                rendezvous_node,
                namespace,
                error,
            } => {
                self.ui.log_error(&format!(
                    "Discovery failed for rendezvous node {}, namespace {:?}: {:?}",
                    rendezvous_node, namespace, error
                ));
                None
            }
            _ => {
                self.ui.log_info(&format!("Got event: {:?}", event));
                None
            }
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

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // env_logger::init();
    simple_logging::log_to_file("trithemius.log", LevelFilter::Debug)?;

    let cli = Cli::parse();

    if cli.register.is_some()
        && cli.discovery.is_some()
        && !cli
            .discovery
            .as_ref()
            .unwrap()
            .contains(&Discovery::Rendezvous)
    {
        Err("Can only register if rendezvous discovery is enabled")?;
    }

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
