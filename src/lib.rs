#[macro_use]
extern crate enum_display_derive;

pub mod cli;
pub mod engine;
pub mod engine_event;
pub mod network_addr;
pub mod subscriptions;
pub mod tor;
pub mod transports;
pub mod util;

pub use crate::cli::{Discovery, NamespaceAndNodeId, NamespaceAndNodeIdParser, NatTraversal};
pub use crate::engine::{Engine, EngineBehaviour, EngineConfig, InputEvent, KademliaType};
use async_trait::async_trait;
use chrono::{DateTime, Local};
use engine_event::EngineEvent;
use futures::stream::FusedStream;
use libp2p::{swarm::NetworkBehaviour, PeerId};

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub date: DateTime<Local>,
    pub topic: String,
    pub user: Option<PeerId>,
    pub message: String,
}

impl ChatMessage {
    pub fn new(user: Option<PeerId>, topic: String, message: String) -> ChatMessage {
        ChatMessage {
            date: Local::now(),
            topic,
            user,
            message,
        }
    }
}

#[async_trait]
pub trait Handler<B: NetworkBehaviour, F: FusedStream> {
    type Event;

    async fn startup(&mut self, engine: &mut Engine) -> Result<(), Box<dyn std::error::Error>>;

    async fn handle_input(
        &mut self,
        engine: &mut Engine,
        line: F::Item,
    ) -> Result<Option<InputEvent>, Box<dyn std::error::Error>>;

    async fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: EngineEvent,
    ) -> Result<Option<EngineEvent>, std::io::Error>;

    async fn handle_error(&mut self, error_message: &str);

    async fn update(&mut self) -> Result<(), std::io::Error>;
}

// #[cfg(test)]
// mod tests {
//     #[test]
//     fn it_works() {
//         let result = 2 + 2;
//         assert_eq!(result, 4);
//     }
// }
