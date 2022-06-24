use crate::ChatMessage;
use libp2p::{
    core::{connection::ListenerId, either::EitherError, ConnectedPoint},
    gossipsub::{error::GossipsubHandlerError, GossipsubEvent, MessageId, TopicHash},
    mdns::MdnsEvent,
    ping::{Failure, PingEvent},
    swarm::{ConnectionError, DialError, PendingConnectionError, SwarmEvent},
    Multiaddr, PeerId, TransportError,
};

use std::num::NonZeroU32;

type HandlerError = EitherError<GossipsubHandlerError, Failure>;

#[derive(Debug)]
pub struct DiscoveredPeer {
    pub peer_id: PeerId,
    pub address: Multiaddr,
}

#[derive(Debug)]
pub enum EngineEvent {
    Message {
        source: Option<PeerId>,
        propagation_source: Option<PeerId>,
        topic: TopicHash,
        message_id: Option<MessageId>,
        sequence_number: Option<u64>,
        message: String,
    },
    ConnectionEstablished {
        peer_id: PeerId,
        endpoint: ConnectedPoint,
        num_established: NonZeroU32,
        concurrent_dial_errors: Option<Vec<(Multiaddr, TransportError<std::io::Error>)>>,
    },
    ConnectionClosed {
        peer_id: PeerId,
        endpoint: ConnectedPoint,
        num_established: u32,
        cause: Option<ConnectionError<HandlerError>>,
    },
    IncomingConnection {
        local_addr: Multiaddr,
        send_back_addr: Multiaddr,
    },
    IncomingConnectionError {
        local_addr: Multiaddr,
        send_back_addr: Multiaddr,
        error: PendingConnectionError<TransportError<std::io::Error>>,
    },
    OutgoingConnectionError {
        peer_id: Option<PeerId>,
        error: DialError,
    },
    BannedPeer {
        peer_id: PeerId,
        endpoint: ConnectedPoint,
    },
    NewListenAddr {
        listener_id: ListenerId,
        address: Multiaddr,
    },
    ExpiredListenAddr {
        listener_id: ListenerId,
        address: Multiaddr,
    },
    ListenerClosed {
        listener_id: ListenerId,
        addresses: Vec<Multiaddr>,
        reason: Result<(), std::io::Error>,
    },
    ListenerError {
        listener_id: ListenerId,
        error: std::io::Error,
    },
    Dialing(PeerId),
    Discovered(Vec<DiscoveredPeer>),
    Expired(Vec<DiscoveredPeer>),
    PingEvent(PingEvent),
    Subscribed {
        peer_id: PeerId,
        topic: TopicHash,
    },
    Unsubscribed {
        peer_id: PeerId,
        topic: TopicHash,
    },
    GossipsubNotSupported {
        peer_id: PeerId,
    },
    Shutdown,
}

impl From<SwarmEvent<EngineEvent, HandlerError>> for EngineEvent {
    fn from(event: SwarmEvent<EngineEvent, HandlerError>) -> Self {
        match event {
            SwarmEvent::Behaviour(engine_event) => engine_event,
            SwarmEvent::BannedPeer { peer_id, endpoint } => {
                EngineEvent::BannedPeer { peer_id, endpoint }
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                concurrent_dial_errors,
            } => EngineEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                concurrent_dial_errors,
            },
            SwarmEvent::ConnectionClosed {
                peer_id,
                endpoint,
                num_established,
                cause,
            } => EngineEvent::ConnectionClosed {
                peer_id,
                endpoint,
                num_established,
                cause,
            },
            SwarmEvent::Dialing(peer_id) => EngineEvent::Dialing(peer_id),
            SwarmEvent::ExpiredListenAddr {
                listener_id,
                address,
            } => EngineEvent::ExpiredListenAddr {
                listener_id,
                address,
            },
            SwarmEvent::IncomingConnection {
                local_addr,
                send_back_addr,
            } => EngineEvent::IncomingConnection {
                local_addr,
                send_back_addr,
            },
            SwarmEvent::IncomingConnectionError {
                local_addr,
                send_back_addr,
                error,
            } => EngineEvent::IncomingConnectionError {
                local_addr,
                send_back_addr,
                error,
            },
            SwarmEvent::ListenerClosed {
                listener_id,
                addresses,
                reason,
            } => EngineEvent::ListenerClosed {
                listener_id,
                addresses,
                reason,
            },
            SwarmEvent::ListenerError { listener_id, error } => {
                EngineEvent::ListenerError { listener_id, error }
            }
            SwarmEvent::NewListenAddr {
                listener_id,
                address,
            } => EngineEvent::NewListenAddr {
                listener_id,
                address,
            },
            SwarmEvent::OutgoingConnectionError { peer_id, error } => {
                EngineEvent::OutgoingConnectionError { peer_id, error }
            }
        }
    }
}

impl From<ChatMessage> for EngineEvent {
    fn from(msg: ChatMessage) -> Self {
        Self::Message {
            source: None,
            propagation_source: None,
            message_id: None,
            topic: TopicHash::from_raw("foo"),
            sequence_number: None,
            message: msg.message,
        }
    }
}
impl From<GossipsubEvent> for EngineEvent {
    fn from(event: GossipsubEvent) -> Self {
        match event {
            GossipsubEvent::Message {
                propagation_source,
                message_id,
                message,
            } => EngineEvent::Message {
                propagation_source: Some(propagation_source),
                message_id: Some(message_id),
                source: message.source,
                topic: message.topic,
                sequence_number: message.sequence_number,
                message: std::str::from_utf8(&message.data).unwrap().to_string(),
            },
            GossipsubEvent::Subscribed { peer_id, topic } => {
                EngineEvent::Subscribed { peer_id, topic }
            }
            GossipsubEvent::Unsubscribed { peer_id, topic } => {
                EngineEvent::Unsubscribed { peer_id, topic }
            }
            GossipsubEvent::GossipsubNotSupported { peer_id } => {
                EngineEvent::GossipsubNotSupported { peer_id }
            }
        }
    }
}

impl From<MdnsEvent> for EngineEvent {
    fn from(event: MdnsEvent) -> Self {
        match event {
            MdnsEvent::Discovered(addrs_iter) => EngineEvent::Discovered(
                addrs_iter
                    .map(|(peer_id, address)| DiscoveredPeer { peer_id, address })
                    .collect(),
            ),
            MdnsEvent::Expired(addrs_iter) => EngineEvent::Expired(
                addrs_iter
                    .map(|(peer_id, address)| DiscoveredPeer { peer_id, address })
                    .collect(),
            ),
        }
    }
}

impl From<PingEvent> for EngineEvent {
    fn from(event: PingEvent) -> Self {
        Self::PingEvent(event)
    }
}
