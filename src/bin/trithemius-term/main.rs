mod config;
mod renderer;
mod state;
mod ui;
mod util;

use crate::state::{CursorMovement, ScrollMovement, State};
use async_trait::async_trait;
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::{future::FutureExt, select, task::Poll};
use futures_lite::stream::StreamExt;
use libp2p::{
    core::either::EitherError,
    floodsub::{self, Floodsub, FloodsubEvent},
    identity,
    mdns::{Mdns, MdnsEvent},
    ping::{Failure, Ping, PingConfig, PingEvent},
    swarm::{ConnectionHandlerUpgrErr, SwarmEvent},
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    Multiaddr,
    NetworkBehaviour,
    PeerId,
};
use log::{debug, error, LevelFilter};
use renderer::Renderer;
use std::pin::Pin;
use std::task::Context;
use tokio::io::{self, AsyncBufReadExt};
use trithemiuslib::{
    create_transport, ChatMessage, Engine, EngineEvent, EventHandler, InputHandler,
};

#[derive(Debug)]
enum Event {
    FloodsubEvent(FloodsubEvent),
    MdnsEvent(MdnsEvent),
    PingEvent(PingEvent),
}

impl From<FloodsubEvent> for Event {
    fn from(event: FloodsubEvent) -> Self {
        Self::FloodsubEvent(event)
    }
}

impl From<MdnsEvent> for Event {
    fn from(event: MdnsEvent) -> Self {
        Self::MdnsEvent(event)
    }
}

impl From<PingEvent> for Event {
    fn from(event: PingEvent) -> Self {
        Self::PingEvent(event)
    }
}

// We create a custom network behaviour that combines floodsub and mDNS.
// The derive generates a delegating `NetworkBehaviour` impl which in turn
// requires the implementations of `NetworkBehaviourEventProcess` for
// the events of each behaviour.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "Event")]
// #[behaviour(event_process = true)]
struct MyBehaviour {
    floodsub: Floodsub,
    mdns: Mdns,
    ping: Ping,
}

struct TermHandler {
    my_identity: PeerId,
    reader: EventStream,
    renderer: Renderer,
    state: State,
    floodsub_topic: floodsub::Topic,
    theme: config::Theme,
}

impl TermHandler {
    fn new(my_identity: PeerId, floodsub_topic: floodsub::Topic) -> Self {
        let mut renderer = Renderer::new();
        let state = State::default();
        let theme = config::Theme::default();
        renderer.render(&state, &theme).unwrap();
        Self {
            my_identity,
            reader: EventStream::new(),
            renderer,
            state,
            floodsub_topic,
            theme,
        }
    }
}

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

#[async_trait]
impl InputHandler<MyBehaviour, TermInputStream> for TermHandler {
    type Event = TermEvent;

    fn handle_network_message(&mut self, chat_message: ChatMessage) -> Result<(), std::io::Error> {
        self.state.add_message(chat_message);
        Ok(())
    }

    fn handle_input(
        &mut self,
        engine: &mut Engine<MyBehaviour>,
        event: Result<Self::Event, std::io::Error>,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        let ret = match event? {
            TermEvent::Mouse(_) => Ok(None),
            TermEvent::Resize(_, _) => Ok(None),
            TermEvent::Key(KeyEvent { code, modifiers }) => match code {
                KeyCode::Esc => Ok(Some(EngineEvent::Shutdown)),
                KeyCode::Char(character) => {
                    if character == 'c' && modifiers.contains(KeyModifiers::CONTROL) {
                        Ok(Some(EngineEvent::Shutdown))
                    } else {
                        self.state.input_write(character);
                        Ok(None)
                    }
                }
                KeyCode::Enter => {
                    if let Some(input) = self.state.reset_input() {
                        let message = ChatMessage::new(self.my_identity, input.clone());
                        self.state.add_message(message);

                        engine
                            .swarm()
                            .behaviour_mut()
                            .floodsub
                            .publish(self.floodsub_topic.clone(), input.as_bytes());
                    }
                    Ok(None)
                }
                KeyCode::Delete => {
                    self.state.input_remove();
                    Ok(None)
                }
                KeyCode::Backspace => {
                    self.state.input_remove_previous();
                    Ok(None)
                }
                KeyCode::Left => {
                    self.state.input_move_cursor(CursorMovement::Left);
                    Ok(None)
                }
                KeyCode::Right => {
                    self.state.input_move_cursor(CursorMovement::Right);
                    Ok(None)
                }
                KeyCode::Home => {
                    self.state.input_move_cursor(CursorMovement::Start);
                    Ok(None)
                }
                KeyCode::End => {
                    self.state.input_move_cursor(CursorMovement::End);
                    Ok(None)
                }
                KeyCode::Up => {
                    self.state.messages_scroll(ScrollMovement::Up);
                    Ok(None)
                }
                KeyCode::Down => {
                    self.state.messages_scroll(ScrollMovement::Down);
                    Ok(None)
                }
                KeyCode::PageUp => {
                    self.state.messages_scroll(ScrollMovement::Start);
                    Ok(None)
                }
                _ => Ok(None),
            },
        };

        self.renderer.render(&mut self.state, &self.theme).unwrap();

        ret
    }
}

struct MyEventHandler {
    known_peers: Vec<PeerId>,
}

impl MyEventHandler {
    fn new() -> Self {
        Self {
            known_peers: Vec::new(),
        }
    }
}

impl EventHandler<MyBehaviour> for MyEventHandler {
    fn handle_event(
        &mut self,
        engine: &mut Engine<MyBehaviour>,
        event: SwarmEvent<
            Event,
            EitherError<EitherError<ConnectionHandlerUpgrErr<std::io::Error>, void::Void>, Failure>,
        >,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        debug!("Event: {:?}", event);
        Ok(match event {
            SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                Event::MdnsEvent(mdns_event) => match mdns_event {
                    MdnsEvent::Discovered(list) => {
                        for (peer, _) in list {
                            if !self.known_peers.contains(&peer) {
                                debug!("Discovered peer {}", peer);
                                engine
                                    .swarm()
                                    .behaviour_mut()
                                    .floodsub
                                    .add_node_to_partial_view(peer);
                                self.known_peers.push(peer);
                            }
                        }
                        None
                    }
                    MdnsEvent::Expired(list) => {
                        for (peer, _) in list {
                            debug!("Peer {} left", peer);
                            let index_opt = self.known_peers.iter().position(|p| *p == peer);
                            if let Some(index) = index_opt {
                                self.known_peers.swap_remove(index);
                            }
                            if !engine.swarm().behaviour().mdns.has_node(&peer) {
                                engine
                                    .swarm()
                                    .behaviour_mut()
                                    .floodsub
                                    .remove_node_from_partial_view(&peer);
                            }
                        }
                        None
                    }
                },
                Event::FloodsubEvent(fs_event) => match fs_event {
                    FloodsubEvent::Message(message) => Some(EngineEvent::ChatMessage(
                        ChatMessage::new(message.source, String::from_utf8(message.data).unwrap()),
                    )),
                    _ => None,
                },
                Event::PingEvent(ping_event) => {
                    debug!("{:?}", ping_event);
                    None
                }
            },
            _ => None,
        })
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

    // Create a Floodsub topic
    let floodsub_topic = floodsub::Topic::new("chat");

    // Create a Swarm to manage peers and events.
    let mut engine = {
        let mdns = Mdns::new(Default::default()).await?;
        let mut behaviour = MyBehaviour {
            floodsub: Floodsub::new(peer_id.clone()),
            mdns,
            ping: Ping::new(PingConfig::new().with_keep_alive(true)),
        };

        behaviour.floodsub.subscribe(floodsub_topic.clone());

        Engine::new(behaviour, transport, peer_id).await
    };

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        let addr: Multiaddr = to_dial.parse().unwrap();
        engine.swarm().dial(addr).unwrap();
        debug!("Dialed {:?}", to_dial);
    }

    let mut term_handler = TermHandler::new(peer_id, floodsub_topic);
    let mut event_handler = MyEventHandler::new();

    // Listen on all interfaces and whatever port the OS assigns
    engine
        .listen("/ip4/0.0.0.0/tcp/0".parse().unwrap())
        .unwrap();

    engine
        .run(TermInputStream::new(), term_handler, event_handler)
        .await
}
