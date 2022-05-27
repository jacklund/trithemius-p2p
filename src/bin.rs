use futures::StreamExt;
use libp2p::{
    floodsub::{self, Floodsub, FloodsubEvent},
    identity,
    mdns::{Mdns, MdnsEvent},
    swarm::{Swarm, SwarmEvent},
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    Multiaddr,
    NetworkBehaviour,
    PeerId,
};
use std::error::Error;
use tokio::io::{self, AsyncBufReadExt};
use trithemiuslib::{create_swarm, create_transport};

#[derive(Debug)]
enum Event {
    FloodsubEvent(FloodsubEvent),
    MdnsEvent(MdnsEvent),
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
}

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    // Create a random PeerId
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    println!("Local peer id: {:?}", peer_id);

    let transport = create_transport(&id_keys);

    // Create a Floodsub topic
    let floodsub_topic = floodsub::Topic::new("chat");

    // Create a Swarm to manage peers and events.
    let mut swarm = {
        let mdns = Mdns::new(Default::default()).await?;
        let mut behaviour = MyBehaviour {
            floodsub: Floodsub::new(peer_id.clone()),
            mdns,
        };

        behaviour.floodsub.subscribe(floodsub_topic.clone());

        create_swarm(behaviour, transport, peer_id).await
    };

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        let addr: Multiaddr = to_dial.parse()?;
        swarm.dial(addr)?;
        println!("Dialed {:?}", to_dial);
    }

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // List of known peers
    let mut known_peers = Vec::<PeerId>::new();

    // Kick it off
    loop {
        tokio::select! {
            line = stdin.next_line() => {
                match line {
                    Ok(Some(string)) => {
                        swarm.behaviour_mut().floodsub.publish(floodsub_topic.clone(), string.as_bytes());
                    },

                    // Ctrl-d
                    Ok(None) => {
                        eprintln!("stdin closed");
                        return Ok(());
                    }
                    Err(error) => {
                        eprintln!("Error on stdin: {}", error);
                    }
                }
            }
            event = swarm.select_next_some() => {
                if let SwarmEvent::NewListenAddr { address, .. } = event {
                    println!("Listening on {:?}", address);
                }
                else if let SwarmEvent::Behaviour(behaviour_event) = event {
                    handle_behaviour_event(behaviour_event, &mut swarm, &mut known_peers);
                }
            }
        }
    }
}

fn handle_behaviour_event(
    behaviour_event: Event,
    swarm: &mut Swarm<MyBehaviour>,
    known_peers: &mut Vec<PeerId>,
) {
    println!("Event: {:?}", behaviour_event);
    match behaviour_event {
        Event::MdnsEvent(mdns_event) => match mdns_event {
            MdnsEvent::Discovered(list) => {
                for (peer, _) in list {
                    if !known_peers.contains(&peer) {
                        println!("Discovered peer {}", peer);
                        if let Err(error) = swarm.dial(peer) {
                            eprintln!("Error connecting to peer {}: {}", peer, error);
                        }
                    }
                    swarm
                        .behaviour_mut()
                        .floodsub
                        .add_node_to_partial_view(peer);
                    known_peers.push(peer);
                }
            }
            MdnsEvent::Expired(list) => {
                for (peer, _) in list {
                    println!("Peer {} left", peer);
                    if !swarm.behaviour().mdns.has_node(&peer) {
                        swarm
                            .behaviour_mut()
                            .floodsub
                            .remove_node_from_partial_view(&peer);
                        if let Err(()) = swarm.disconnect_peer_id(peer) {
                            eprintln!("Error disconnecting from peer {}: Not connected", peer);
                        }
                        let index = known_peers.iter().position(|p| *p == peer);
                        if let Some(index) = index {
                            known_peers.remove(index);
                        }
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
    }
}
