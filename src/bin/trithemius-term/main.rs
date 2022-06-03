mod config;
mod renderer;
mod state;
mod ui;
mod util;

use crate::state::{ChatMessage, CursorMovement, MessageType, ScrollMovement, State};
use async_trait::async_trait;
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::{future::FutureExt, select, StreamExt};
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
use renderer::Renderer;
use tokio::io::{self, AsyncBufReadExt};
use trithemiuslib::{create_transport, Engine, EventHandler, InputHandler};

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
    reader: EventStream,
    renderer: Renderer,
    state: State,
    floodsub_topic: floodsub::Topic,
}

impl TermHandler {
    fn new(floodsub_topic: floodsub::Topic) -> Self {
        Self {
            reader: EventStream::new(),
            renderer: Renderer::new(),
            state: State::default(),
            floodsub_topic,
        }
    }
}

#[async_trait]
impl InputHandler<MyBehaviour> for TermHandler {
    type Event = TermEvent;

    async fn get_input(&mut self) -> Option<io::Result<Self::Event>> {
        let mut event = self.reader.next().fuse();
        event.await
    }

    fn handle_input(
        &mut self,
        engine: &mut Engine<MyBehaviour>,
        event: Option<Self::Event>,
    ) -> Result<(), std::io::Error> {
        match event {
            Some(TermEvent::Mouse(_)) => Ok(()),
            Some(TermEvent::Resize(_, _)) => Ok(()),
            Some(TermEvent::Key(KeyEvent { code, modifiers })) => match code {
                // KeyCode::Esc => self.node.signals().send_with_priority(Signal::Close(None)),
                KeyCode::Char(character) => {
                    // if character == 'c' && modifiers.contains(KeyModifiers::CONTROL) {
                    //     self.node.signals().send_with_priority(Signal::Close(None))
                    // } else {
                    self.state.input_write(character);
                    Ok(())
                    // }
                }
                KeyCode::Enter => {
                    if let Some(input) = self.state.reset_input() {
                        let message = ChatMessage::new(
                            "me".to_string(),
                            // format!("{} (me)", self.config.user_name),
                            MessageType::Text(input.clone()),
                        );
                        self.state.add_message(message);

                        engine
                            .swarm()
                            .behaviour_mut()
                            .floodsub
                            .publish(self.floodsub_topic.clone(), input.clone().as_bytes());
                    }
                    Ok(())
                }
                KeyCode::Delete => Ok(self.state.input_remove()),
                KeyCode::Backspace => Ok(self.state.input_remove_previous()),
                KeyCode::Left => Ok(self.state.input_move_cursor(CursorMovement::Left)),
                KeyCode::Right => Ok(self.state.input_move_cursor(CursorMovement::Right)),
                KeyCode::Home => Ok(self.state.input_move_cursor(CursorMovement::Start)),
                KeyCode::End => Ok(self.state.input_move_cursor(CursorMovement::End)),
                KeyCode::Up => Ok(self.state.messages_scroll(ScrollMovement::Up)),
                KeyCode::Down => Ok(self.state.messages_scroll(ScrollMovement::Down)),
                KeyCode::PageUp => Ok(self.state.messages_scroll(ScrollMovement::Start)),
                _ => Ok(()),
            },
            None => Ok(()),
        }
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
    ) -> Result<(), std::io::Error> {
        println!("Event: {:?}", event);
        match event {
            SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                Event::MdnsEvent(mdns_event) => match mdns_event {
                    MdnsEvent::Discovered(list) => {
                        for (peer, _) in list {
                            if !self.known_peers.contains(&peer) {
                                println!("Discovered peer {}", peer);
                                engine
                                    .swarm()
                                    .behaviour_mut()
                                    .floodsub
                                    .add_node_to_partial_view(peer);
                                self.known_peers.push(peer);
                            }
                        }
                    }
                    MdnsEvent::Expired(list) => {
                        for (peer, _) in list {
                            println!("Peer {} left", peer);
                            if !engine.swarm().behaviour().mdns.has_node(&peer) {
                                engine
                                    .swarm()
                                    .behaviour_mut()
                                    .floodsub
                                    .remove_node_from_partial_view(&peer);
                            }
                        }
                    }
                },
                Event::FloodsubEvent(fs_event) => match fs_event {
                    FloodsubEvent::Message(message) => {
                        println!(
                            "Received: '{:?}' from {:?}",
                            String::from_utf8_lossy(&message.data),
                            message.source
                        );
                    }
                    _ => (),
                },
                Event::PingEvent(_ping_event) => {
                    // println!("{:?}", ping_event);
                }
            },
            _ => (),
        }

        Ok(())
    }
}

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    env_logger::init();

    // Create a random PeerId
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    println!("Local peer id: {:?}", peer_id);

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
        println!("Dialed {:?}", to_dial);
    }

    let mut term_handler = TermHandler::new(floodsub_topic);
    let mut event_handler = MyEventHandler::new();

    // Listen on all interfaces and whatever port the OS assigns
    engine
        .listen("/ip4/0.0.0.0/tcp/0".parse().unwrap())
        .unwrap();

    engine.run(term_handler, event_handler).await
}
