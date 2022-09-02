use crate::ChatMessage;
use libp2p::{
    autonat::{Event as AutonatEvent, InboundProbeEvent, NatStatus, OutboundProbeEvent},
    core::{either::EitherError, transport::ListenerId, ConnectedPoint},
    gossipsub::{error::GossipsubHandlerError, GossipsubEvent, MessageId, TopicHash},
    identify::{IdentifyEvent, IdentifyInfo, UpgradeError as IdentifyUpgradeError},
    kad::{
        kbucket::Distance, Addresses, InboundRequest, KademliaEvent, QueryId, QueryResult,
        QueryStats,
    },
    mdns::MdnsEvent,
    ping::{Failure, PingEvent},
    swarm::{
        ConnectionError, ConnectionHandlerUpgrErr, DialError, PendingConnectionError, SwarmEvent,
    },
    Multiaddr, PeerId, TransportError,
};
use libp2p_dcutr::{
    behaviour::{Event as DcutrEvent, UpgradeError as DcutrUpgradeError},
    InboundUpgradeError, OutboundUpgradeError,
};

use std::num::NonZeroU32;

type HandlerError = EitherError<
    EitherError<
        EitherError<
            EitherError<
                EitherError<
                    EitherError<GossipsubHandlerError, Failure>,
                    ConnectionHandlerUpgrErr<std::io::Error>,
                >,
                either::Either<
                    ConnectionHandlerUpgrErr<
                        EitherError<InboundUpgradeError, OutboundUpgradeError>,
                    >,
                    either::Either<ConnectionHandlerUpgrErr<std::io::Error>, void::Void>,
                >,
            >,
            std::io::Error,
        >,
        void::Void,
    >,
    std::io::Error,
>;

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
    PeerAddresses {
        peer: PeerId,
        addresses: Vec<Multiaddr>,
    },

    // SwarmEvents
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

    // MDNS
    MdnsDiscovered(Vec<DiscoveredPeer>),
    MdnsExpired(Vec<DiscoveredPeer>),

    // Ping
    PingEvent(PingEvent),

    // Gossipsub
    GossipSubscribed {
        peer_id: PeerId,
        topic: TopicHash,
    },
    GossipUnsubscribed {
        peer_id: PeerId,
        topic: TopicHash,
    },
    GossipsubNotSupported {
        peer_id: PeerId,
    },

    // Identify
    IdentifyReceived {
        peer_id: PeerId,
        info: IdentifyInfo,
    },
    IdentifySent {
        peer_id: PeerId,
    },
    IdentifyPushed {
        peer_id: PeerId,
    },
    IdentifyError {
        peer_id: PeerId,
        error: ConnectionHandlerUpgrErr<IdentifyUpgradeError>,
    },

    // DCUTR
    InitiatedDirectConnectionUpgrade {
        remote_peer_id: PeerId,
        local_relayed_addr: Multiaddr,
    },
    RemoteInitiatedDirectConnectionUpgrade {
        remote_peer_id: PeerId,
        remote_relayed_addr: Multiaddr,
    },
    DirectConnectionUpgradeSucceeded {
        remote_peer_id: PeerId,
    },
    DirectConnectionUpgradeFailed {
        remote_peer_id: PeerId,
        error: DcutrUpgradeError,
    },

    // Autonat
    InboundProbe(InboundProbeEvent),
    OutboundProbe(OutboundProbeEvent),
    AutonatStatusChanged {
        old: NatStatus,
        new: NatStatus,
    },

    // Kademlia
    KadInboundRequest {
        request: InboundRequest,
    },
    KadOutboundQueryCompleted {
        id: QueryId,
        result: QueryResult,
        stats: QueryStats,
    },
    KadRoutingUpdated {
        peer: PeerId,
        is_new_peer: bool,
        addresses: Addresses,
        bucket_range: (Distance, Distance),
        old_peer: Option<PeerId>,
    },
    KadUnroutablePeer {
        peer: PeerId,
    },
    KadRoutablePeer {
        peer: PeerId,
        address: Multiaddr,
    },
    KadPendingRoutablePeer {
        peer: PeerId,
        address: Multiaddr,
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

impl From<DcutrEvent> for EngineEvent {
    fn from(event: DcutrEvent) -> Self {
        match event {
            DcutrEvent::InitiatedDirectConnectionUpgrade {
                remote_peer_id,
                local_relayed_addr,
            } => EngineEvent::InitiatedDirectConnectionUpgrade {
                remote_peer_id,
                local_relayed_addr,
            },
            DcutrEvent::RemoteInitiatedDirectConnectionUpgrade {
                remote_peer_id,
                remote_relayed_addr,
            } => EngineEvent::RemoteInitiatedDirectConnectionUpgrade {
                remote_peer_id,
                remote_relayed_addr,
            },
            DcutrEvent::DirectConnectionUpgradeSucceeded { remote_peer_id } => {
                EngineEvent::DirectConnectionUpgradeSucceeded { remote_peer_id }
            }
            DcutrEvent::DirectConnectionUpgradeFailed {
                remote_peer_id,
                error,
            } => EngineEvent::DirectConnectionUpgradeFailed {
                remote_peer_id,
                error,
            },
        }
    }
}

impl From<AutonatEvent> for EngineEvent {
    fn from(event: AutonatEvent) -> Self {
        match event {
            AutonatEvent::InboundProbe(inbound_event) => EngineEvent::InboundProbe(inbound_event),
            AutonatEvent::OutboundProbe(outbound_event) => {
                EngineEvent::OutboundProbe(outbound_event)
            }
            AutonatEvent::StatusChanged { old, new } => {
                EngineEvent::AutonatStatusChanged { old, new }
            }
        }
    }
}

impl From<IdentifyEvent> for EngineEvent {
    fn from(event: IdentifyEvent) -> Self {
        match event {
            IdentifyEvent::Received { peer_id, info } => Self::IdentifyReceived { peer_id, info },
            IdentifyEvent::Sent { peer_id } => Self::IdentifySent { peer_id },
            IdentifyEvent::Pushed { peer_id } => Self::IdentifyPushed { peer_id },
            IdentifyEvent::Error { peer_id, error } => Self::IdentifyError { peer_id, error },
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
                message: std::str::from_utf8(&message.data).unwrap().to_string(), // TODO: Figure out how not to unwrap this
            },
            GossipsubEvent::Subscribed { peer_id, topic } => {
                EngineEvent::GossipSubscribed { peer_id, topic }
            }
            GossipsubEvent::Unsubscribed { peer_id, topic } => {
                EngineEvent::GossipUnsubscribed { peer_id, topic }
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
            MdnsEvent::Discovered(addrs_iter) => EngineEvent::MdnsDiscovered(
                addrs_iter
                    .map(|(peer_id, address)| DiscoveredPeer { peer_id, address })
                    .collect(),
            ),
            MdnsEvent::Expired(addrs_iter) => EngineEvent::MdnsExpired(
                addrs_iter
                    .map(|(peer_id, address)| DiscoveredPeer { peer_id, address })
                    .collect(),
            ),
        }
    }
}

impl From<KademliaEvent> for EngineEvent {
    fn from(event: KademliaEvent) -> Self {
        match event {
            KademliaEvent::InboundRequest { request } => EngineEvent::KadInboundRequest { request },
            KademliaEvent::OutboundQueryCompleted { id, result, stats } => {
                EngineEvent::KadOutboundQueryCompleted { id, result, stats }
            }
            KademliaEvent::RoutingUpdated {
                peer,
                is_new_peer,
                addresses,
                bucket_range,
                old_peer,
            } => EngineEvent::KadRoutingUpdated {
                peer,
                is_new_peer,
                addresses,
                bucket_range,
                old_peer,
            },
            KademliaEvent::UnroutablePeer { peer } => EngineEvent::KadUnroutablePeer { peer },
            KademliaEvent::RoutablePeer { peer, address } => {
                EngineEvent::KadRoutablePeer { peer, address }
            }
            KademliaEvent::PendingRoutablePeer { peer, address } => {
                EngineEvent::KadPendingRoutablePeer { peer, address }
            }
        }
    }
}

impl From<PingEvent> for EngineEvent {
    fn from(event: PingEvent) -> Self {
        Self::PingEvent(event)
    }
}
