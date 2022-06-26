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

    fn handle_input(
        &mut self,
        engine: &mut Engine,
        event: Result<Self::Event, std::io::Error>,
    ) -> Result<Option<InputEvent>, std::io::Error> {
        let event = self.ui.handle_input_event(engine, event?)?;
        debug!("handle_input, got event {:?}", event);

        Ok(event)
    }

    fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: EngineEvent,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        debug!("Event: {:?}", event);
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
            _ => None,
        })
    }

    fn handle_error(&mut self, error_message: &str) {
        self.ui.print_error(error_message);
    }

    fn update(&mut self) -> Result<(), std::io::Error> {
        debug!("Called handler::update()");
        self.renderer.render(&self.ui)
    }
}

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    // env_logger::init();
    simple_logging::log_to_file("trithemius.log", LevelFilter::Debug)?;

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
    let handler = MyHandler::new(peer_id, topic.clone());

    // Listen on all interfaces and whatever port the OS assigns
    engine
        .listen("/ip4/0.0.0.0/tcp/0".parse().unwrap())
        .unwrap();

    engine.subscribe("chat").unwrap();

    engine.run(TermInputStream::new(), handler).await
}
