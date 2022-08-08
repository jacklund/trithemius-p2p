use crate::ui::{Renderer, UI};
use async_trait::async_trait;
use crossterm::event::{Event as TermEvent, EventStream};
use futures::task::Poll;
use futures_lite::stream::StreamExt;
use libp2p::{
    gossipsub::IdentTopic,
    identity,
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    Multiaddr,
    PeerId,
};
use log::{debug, LevelFilter};
use simple_logging;
use std::pin::Pin;
use std::task::Context;
use trithemiuslib::{
    create_transport, engine_event::EngineEvent, ChatMessage, Engine, EngineBehaviour, Handler,
    InputEvent,
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
    ui: UI,
    known_peers: Vec<PeerId>,
    renderer: Renderer,
}

impl MyHandler {
    fn new(my_identity: PeerId, topic: IdentTopic) -> Self {
        Self {
            my_identity,
            ui: UI::new(my_identity),
            known_peers: Vec::new(),
            renderer: Renderer::new(),
        }
    }
}

#[async_trait]
impl Handler<EngineBehaviour, TermInputStream> for MyHandler {
    type Event = TermEvent;

    async fn handle_input(
        &mut self,
        engine: &mut Engine,
        event: Result<Self::Event, std::io::Error>,
    ) -> Result<Option<InputEvent>, std::io::Error> {
        let event = self.ui.handle_input_event(engine, event?).await?;
        // debug!("handle_input, got event {:?}", event);

        Ok(event)
    }

    async fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: EngineEvent,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        // debug!("Event: {:?}", event);
        Ok(match event {
            EngineEvent::Discovered(list) => {
                for peer in list {
                    debug!("Discovered peer {}", peer.peer_id);
                }
                None
            }
            EngineEvent::Expired(list) => {
                for peer in list {
                    debug!("Peer {} left", peer.peer_id);
                }
                None
            }
            EngineEvent::Message {
                source,
                propagation_source,
                topic,
                message_id,
                sequence_number,
                message,
            } => {
                self.ui
                    .add_message(ChatMessage::new(source.unwrap(), message));
                None
            }
            EngineEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established: _,
                concurrent_dial_errors: _,
            } => {
                self.ui
                    .log_info(&format!("Connected to {} at {:?}", peer_id, endpoint));
                None
            }
            EngineEvent::ConnectionClosed {
                peer_id,
                endpoint,
                num_established: _,
                cause,
            } => {
                self.ui.log_info(&format!(
                    "Connection closed to {} at {:?}: {:?}",
                    peer_id, endpoint, cause
                ));
                None
            }
            EngineEvent::IncomingConnection {
                local_addr,
                send_back_addr,
            } => {
                self.ui.log_info(&format!(
                    "Incoming connection to {} from {}",
                    local_addr, send_back_addr,
                ));
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
            EngineEvent::Discovered(peers) => {
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
            EngineEvent::Subscribed { peer_id, topic } => {
                self.ui
                    .log_info(&format!("Peer {} subscribed to {}", peer_id, topic));
                None
            }
            EngineEvent::Unsubscribed { peer_id, topic } => {
                self.ui
                    .log_info(&format!("Peer {} unsubscribed to {}", peer_id, topic));
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

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    // env_logger::init();
    // simple_logging::log_to_file("trithemius.log", LevelFilter::Debug)?;

    // Create a random PeerId
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    debug!("Local peer id: {:?}", peer_id);

    let transport = create_transport(&id_keys);

    // Create a Swarm to manage peers and events.
    let mut engine = Engine::new(id_keys, transport, peer_id)?;

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        let addr: Multiaddr = to_dial.parse().unwrap();
        engine.dial(addr).unwrap();
        debug!("Dialed {:?}", to_dial);
    }

    let topic = IdentTopic::new("chat");
    let mut handler = MyHandler::new(peer_id, topic.clone());
    handler
        .ui
        .log_info(&format!("Local peer id: {:?}", peer_id));

    engine.run(TermInputStream::new(), handler).await
}
