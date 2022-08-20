use crate::ChatMessage;
use circular_queue::CircularQueue;
use std::collections::{hash_map, HashMap};

#[derive(Debug)]
pub enum SubscriptionError {
    NoSuchTopic(String),
}

impl std::fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SubscriptionError::NoSuchTopic(topic) => write!(f, "Not subscribed to topic {}", topic),
        }
    }
}

#[derive(Debug)]
pub struct Subscription {
    pub topic: String,
    pub messages: CircularQueue<ChatMessage>,
}

impl Subscription {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_string(),
            messages: CircularQueue::with_capacity(200), // TODO: Configure this
        }
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }
}

impl PartialEq for Subscription {
    fn eq(&self, other: &Self) -> bool {
        self.topic == other.topic
    }
}

#[derive(Debug, PartialEq)]
pub struct Subscriptions {
    subscriptions: HashMap<String, Subscription>,
}

impl Subscriptions {
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
        }
    }

    pub fn iter(&self) -> hash_map::Iter<String, Subscription> {
        self.subscriptions.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    pub fn add(&mut self, topic: &str) {
        self.subscriptions
            .insert(topic.to_string(), Subscription::new(topic));
    }

    pub fn remove(&mut self, topic: &str) -> Option<Subscription> {
        self.subscriptions.remove(topic)
    }

    pub fn get(&self, topic_name: &str) -> Option<&Subscription> {
        self.subscriptions.get(topic_name)
    }

    pub fn get_mut(&mut self, topic_name: &str) -> Option<&mut Subscription> {
        self.subscriptions.get_mut(topic_name)
    }

    pub fn add_message(
        &mut self,
        topic: &str,
        message: ChatMessage,
    ) -> Result<(), SubscriptionError> {
        match self.get_mut(topic) {
            Some(subscription) => Ok(subscription.add_message(message)),
            None => Err(SubscriptionError::NoSuchTopic(topic.to_string())),
        }
    }
}
