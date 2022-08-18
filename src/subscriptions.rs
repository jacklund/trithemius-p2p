use crate::ChatMessage;
use circular_queue::CircularQueue;

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

pub struct Subscriptions {
    subscriptions: Vec<Subscription>,
    current_subscription_index: Option<usize>,
}

impl Subscriptions {
    pub fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
            current_subscription_index: None,
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Subscription> {
        self.subscriptions.iter()
    }

    pub fn next(&mut self) -> Option<&mut Subscription> {
        match self.current_subscription_index {
            Some(index) => {
                if index == self.subscriptions.len() - 1 {
                    self.current_subscription_index = Some(0);
                } else {
                    self.current_subscription_index = Some(index + 1);
                }
                self.current_mut()
            }
            None => None,
        }
    }

    pub fn prev(&mut self) -> Option<&mut Subscription> {
        match self.current_subscription_index {
            Some(index) => {
                if index == 0 {
                    self.current_subscription_index = Some(self.subscriptions.len() - 1);
                } else {
                    self.current_subscription_index = Some(index - 1);
                }
                self.current_mut()
            }
            None => None,
        }
    }

    pub fn current(&self) -> Option<&Subscription> {
        match self.current_subscription_index {
            Some(index) => self.subscriptions.get(index),
            None => None,
        }
    }

    pub fn current_mut(&mut self) -> Option<&mut Subscription> {
        match self.current_subscription_index {
            Some(index) => self.subscriptions.get_mut(index),
            None => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current_subscription_index
    }

    pub fn add(&mut self, topic: &str) {
        self.subscriptions.push(Subscription::new(topic));
        self.current_subscription_index = Some(self.subscriptions.len() - 1);
    }

    pub fn remove(&mut self, topic: &str) -> Option<Subscription> {
        match self.get_index(topic) {
            Some(index) => {
                let subscription = self.subscriptions.swap_remove(index);
                match self.current_subscription_index {
                    Some(current) => {
                        if current > index {
                            self.current_subscription_index = Some(current - 1);
                        }
                        if current == index {
                            if current > 0 {
                                self.current_subscription_index = Some(current - 1);
                            } else {
                                if self.subscriptions.is_empty() {
                                    self.current_subscription_index = None;
                                }
                            }
                        }
                        Some(subscription)
                    }
                    None => panic!("This shouldn't happen!"),
                }
            }
            None => None,
        }
    }

    fn get_index(&self, topic_name: &str) -> Option<usize> {
        self.subscriptions
            .iter()
            .position(|t| t.topic == topic_name)
    }

    pub fn get(&mut self, topic_name: &str) -> Option<&mut Subscription> {
        match self.get_index(topic_name) {
            Some(index) => self.subscriptions.get_mut(index),
            None => None,
        }
    }
}
