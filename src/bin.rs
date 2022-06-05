use async_trait::async_trait;
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
use tokio::io::{self, AsyncBufReadExt};
use trithemiuslib::{create_transport, Engine, EngineEvent, EventHandler, InputHandler};
use void;

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

struct StdinHandler {
    stdin: io::Lines<io::BufReader<io::Stdin>>,
    floodsub_topic: floodsub::Topic,
}

impl StdinHandler {
    fn new(floodsub_topic: floodsub::Topic) -> Self {
        Self {
            stdin: io::BufReader::new(io::stdin()).lines(),
            floodsub_topic,
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
    ) -> Result<Option<EngineEvent>, std::io::Error> {
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

        Ok(None)
    }
}

#[async_trait]
impl InputHandler<MyBehaviour> for StdinHandler {
    type Event = String;

    async fn get_input(&mut self) -> Option<io::Result<Self::Event>> {
        self.stdin.next_line().await.transpose()
    }

    fn handle_network_message(
        &mut self,
        peer_id: PeerId,
        message: String,
    ) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn handle_input(
        &mut self,
        engine: &mut Engine<MyBehaviour>,
        line: Option<Self::Event>,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        match line {
            Some(string) => {
                engine
                    .swarm()
                    .behaviour_mut()
                    .floodsub
                    .publish(self.floodsub_topic.clone(), string.as_bytes());
                Ok(None)
            }

            // Ctrl-d
            None => {
                eprintln!("stdin closed");
                Ok(None)
            }
        }
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

    let stdin_handler = StdinHandler::new(floodsub_topic);
    let event_handler = MyEventHandler::new();

    // Listen on all interfaces and whatever port the OS assigns
    engine
        .listen("/ip4/0.0.0.0/tcp/0".parse().unwrap())
        .unwrap();

    engine.run(stdin_handler, event_handler).await
}
