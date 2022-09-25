use chrono::{DateTime, Local};
use circular_queue::CircularQueue;
use clap::{crate_name, crate_version};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use crossterm::ExecutableCommand;
use libp2p::gossipsub::IdentTopic;
use libp2p::rendezvous::Namespace;
use libp2p::{core::transport::ListenerId, Multiaddr, PeerId};
use log::debug;
use std::str::FromStr;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use trithemiuslib::{
    cli::{Discovery, NatTraversal},
    network_addr::NetworkAddress,
    subscriptions::{SubscriptionError, Subscriptions},
    ChatMessage, Engine, InputEvent,
};

use tui::backend::CrosstermBackend;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::Color;
use tui::style::{Modifier, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, Paragraph, Tabs, Wrap};
use tui::Frame;
use tui::Terminal;

use std::collections::{HashMap, VecDeque};
use std::io::Write;

enum Level {
    Info,
    Warning,
    Error,
}

struct LogMessage {
    date: DateTime<Local>,
    level: Level,
    message: String,
}

pub enum CursorMovement {
    Left,
    Right,
    Start,
    End,
}

pub enum ScrollMovement {
    Up,
    Down,
    Start,
}

// split messages to fit the width of the ui panel
pub fn split_each(input: String, width: usize) -> Vec<String> {
    let mut splitted = Vec::with_capacity(input.width() / width);
    let mut row = String::new();

    let mut index = 0;

    for current_char in input.chars() {
        if (index != 0 && index == width) || index + current_char.width().unwrap_or(0) > width {
            splitted.push(row.drain(..).collect());
            index = 0;
        }

        row.push(current_char);
        index += current_char.width().unwrap_or(0);
    }
    // leftover
    if !row.is_empty() {
        splitted.push(row.drain(..).collect());
    }
    splitted
}

pub struct Renderer {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl Renderer {
    pub fn new() -> Self {
        terminal::enable_raw_mode().expect("Error: unable to put terminal in raw mode");
        let mut out = std::io::stdout();
        out.execute(terminal::EnterAlternateScreen).unwrap();

        Self {
            terminal: Terminal::new(CrosstermBackend::new(out)).unwrap(),
        }
    }

    pub fn render(&mut self, ui: &UI) -> Result<(), std::io::Error> {
        self.terminal.draw(|frame| ui.draw(frame, frame.size()))?;
        Ok(())
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.terminal
            .backend_mut()
            .execute(terminal::LeaveAlternateScreen)
            .expect("Could not execute LeaveAlternateScreen");
        terminal::disable_raw_mode().expect("Failed disabling raw mode");
    }
}

struct SubscriptionList {
    list: Vec<String>,
    current_index: Option<usize>,
}

impl SubscriptionList {
    fn new() -> Self {
        Self {
            list: Vec::new(),
            current_index: None,
        }
    }

    fn contains(&self, topic: &str) -> bool {
        self.list.contains(&topic.to_string())
    }

    fn names(&self) -> &Vec<String> {
        &self.list
    }

    fn subscribe(&mut self, topic: &str) {
        self.list.push(topic.to_string());
        self.current_index = Some(self.list.len() - 1);
    }

    fn unsubscribe(&mut self, topic: &str) -> Result<(), SubscriptionError> {
        match self.list.iter().position(|t| t == topic) {
            Some(index) => {
                self.list.swap_remove(index);
                if self.list.is_empty() {
                    self.current_index = None;
                } else {
                    match self.current_index {
                        Some(current) => {
                            if current >= index {
                                self.current_index = Some(current - 1);
                            }
                        }
                        None => {
                            panic!("Current subscription index is None when it shouldn't be");
                        }
                    }
                }
                Ok(())
            }
            None => Err(SubscriptionError::NoSuchTopic(topic.to_string())),
        }
    }

    fn current(&self) -> Option<&String> {
        match self.current_index {
            Some(index) => self.list.get(index),
            None => None,
        }
    }

    fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    fn next(&mut self) -> Option<&String> {
        match self.current_index {
            Some(index) => {
                if index == self.list.len() - 1 {
                    self.current_index = Some(0);
                } else {
                    self.current_index = Some(index + 1);
                }
                self.current()
            }
            None => None,
        }
    }

    fn prev(&mut self) -> Option<&String> {
        match self.current_index {
            Some(index) => {
                if index == 0 {
                    self.current_index = Some(self.list.len() - 1);
                } else {
                    self.current_index = Some(index - 1);
                }
                self.current()
            }
            None => None,
        }
    }
}

pub struct UI {
    my_identity: PeerId,
    log_messages: CircularQueue<LogMessage>,
    subscriptions: Subscriptions,
    subscription_list: SubscriptionList,
    connect_list: Vec<Multiaddr>,
    scroll_messages_view: usize,
    input: Vec<char>,
    input_cursor: usize,
    user_ids: HashMap<PeerId, usize>,
    last_user_id: usize,
    message_colors: Vec<Color>,
    my_user_color: Color,
    date_color: Color,
    chat_panel_color: Color,
    input_panel_color: Color,
    discovery_methods: String,
    nat_traversal_methods: String,
}

impl UI {
    pub fn new(my_identity: PeerId) -> Self {
        Self {
            my_identity,
            log_messages: CircularQueue::with_capacity(200),
            subscriptions: Subscriptions::default(),
            subscription_list: SubscriptionList::new(),
            connect_list: Vec::new(),
            scroll_messages_view: 0,
            input: Vec::new(),
            input_cursor: 0,
            user_ids: HashMap::new(),
            last_user_id: 0,
            message_colors: vec![Color::Blue, Color::Yellow, Color::Cyan, Color::Magenta],
            my_user_color: Color::Green,
            date_color: Color::DarkGray,
            chat_panel_color: Color::White,
            input_panel_color: Color::White,
            discovery_methods: String::new(),
            nat_traversal_methods: String::new(),
        }
    }

    pub fn startup(
        &mut self,
        discovery_methods: &mut Vec<Discovery>,
        nat_traversal_methods: &mut Vec<NatTraversal>,
    ) {
        self.discovery_methods = if discovery_methods.is_empty() {
            "None".to_string()
        } else {
            discovery_methods
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        };

        self.nat_traversal_methods = if nat_traversal_methods.is_empty() {
            "None".to_string()
        } else {
            nat_traversal_methods
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        };
    }

    pub fn scroll_messages_view(&self) -> usize {
        self.scroll_messages_view
    }

    pub fn ui_input_cursor(&self, width: usize) -> (u16, u16) {
        let mut position = (0, 0);

        for current_char in self.input.iter().take(self.input_cursor) {
            let char_width = unicode_width::UnicodeWidthChar::width(*current_char).unwrap_or(0);

            position.0 += char_width;

            match position.0.cmp(&width) {
                std::cmp::Ordering::Equal => {
                    position.0 = 0;
                    position.1 += 1;
                }
                std::cmp::Ordering::Greater => {
                    // Handle a char with width > 1 at the end of the row
                    // width - (char_width - 1) accounts for the empty column(s) left behind
                    position.0 -= width - (char_width - 1);
                    position.1 += 1;
                }
                _ => (),
            }
        }

        (position.0 as u16, position.1 as u16)
    }

    pub fn subscribed(&self, topic_name: &str) -> bool {
        self.subscription_list.contains(topic_name)
    }

    pub fn subscribe(&mut self, engine: &mut Engine, topic_name: &str) {
        match engine.subscribe(topic_name) {
            Ok(true) => {
                self.subscriptions.add(topic_name);
                self.subscription_list.subscribe(topic_name);
                self.log_info(&format!("Subscribed to topic '{}'", topic_name))
            }
            Ok(false) => self.log_info(&format!("Already subscribed to topic '{}'", topic_name)),
            Err(error) => self.log_error(&format!(
                "Error subscribing to topic {}: {}",
                topic_name, error
            )),
        };
    }

    pub fn unsubscribe(&mut self, engine: &mut Engine, topic_name: &str) {
        match engine.unsubscribe(topic_name) {
            Ok(true) => match self.subscriptions.remove(topic_name) {
                Some(_) => {
                    self.log_info(&format!("Unsubscribed to topic '{}'", topic_name));
                    if let Err(error) = self.subscription_list.unsubscribe(topic_name) {
                        self.log_error(&format!("Error unsubscribing: {}", error));
                    }
                }
                None => self.log_error(&format!("Not subscribed to topic '{}'", topic_name)),
            },
            Ok(false) => self.log_info(&format!("Not subscribed to topic '{}'", topic_name)),
            Err(error) => self.log_error(&format!(
                "Error unsubscribing from topic {}: {}",
                topic_name, error
            )),
        };
    }

    pub fn listen(
        &mut self,
        engine: &mut Engine,
        address: &str,
    ) -> Result<ListenerId, Box<dyn std::error::Error>> {
        match address.parse::<NetworkAddress>() {
            Ok(network_addr) => Ok(engine.listen(network_addr.into())?),
            Err(error) => {
                self.log_error(&format!("Error parsing network address: {}", error));
                Err(error)?
            }
        }
    }

    pub fn connect(
        &mut self,
        engine: &mut Engine,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match address.parse::<NetworkAddress>() {
            Ok(network_addr) => {
                self.log_info(&format!("Connecting to {}", network_addr));
                engine.dial(network_addr.clone().into())?;
                self.connect_list.push(network_addr.into());
            }
            Err(error) => {
                self.log_error(&format!("Error parsing network address: {}", error));
                Err(error)?
            }
        };
        Ok(())
    }

    pub fn register(
        &mut self,
        engine: &mut Engine,
        namespace: &str,
        rendezvous_node: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rendezvous_node = PeerId::from_str(rendezvous_node)?;
        let namespace = Namespace::new(namespace.to_string())?;
        Ok(engine.register(namespace, rendezvous_node)?)
    }

    pub async fn find_peer(
        &mut self,
        engine: &mut Engine,
        peer_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match PeerId::from_str(peer_id) {
            Ok(peer) => {
                self.log_info(&format!("Finding peer {}", peer));
                engine.find_peer(&peer).await;
            }
            Err(error) => {
                self.log_error(&format!("Error parsing PeerId: {}", error));
                Err(error)?
            }
        };
        Ok(())
    }

    pub fn connecting_to(&self, address: &Multiaddr) -> bool {
        self.connect_list.contains(address)
    }

    pub fn disconnected_from(&mut self, address: &Multiaddr) {
        match self.connect_list.iter().position(|a| a == address) {
            Some(index) => {
                self.connect_list.swap_remove(index);
            }
            None => (),
        };
    }

    pub async fn create_onion_service(
        &mut self,
        engine: &mut Engine,
        virt_port_str: &str,
        listen_address: Option<&str>,
    ) {
        let (virt_port, listen_address) = match virt_port_str.parse::<u16>() {
            Ok(virt_port) => match listen_address {
                Some(address_string) => match Multiaddr::from_str(address_string) {
                    Ok(listen_address) => (virt_port, listen_address),
                    Err(error) => {
                        self.log_error(&format!("Error parsing listen address: {}", error));
                        return;
                    }
                },
                None => (
                    virt_port,
                    Multiaddr::from_str(&format!("/ip4/127.0.0.1/tcp/{}", virt_port)).unwrap(),
                ),
            },
            Err(error) => {
                self.log_error(&format!("Error parsing virtual port: {}", error));
                return;
            }
        };

        match engine
            .create_transient_onion_service(virt_port, listen_address)
            .await
        {
            Ok(onion_service) => {
                self.log_info(&format!("Created onion service {}", onion_service.address,));
            }
            Err(error) => {
                self.log_error(&format!("Error creating onion service: {}", error));
            }
        }
    }

    async fn handle_command<'a>(
        &mut self,
        engine: &mut Engine,
        mut command_args: VecDeque<&'a str>,
    ) -> Result<Option<InputEvent>, Box<dyn std::error::Error>> {
        let command = command_args.pop_front();
        match command {
            Some(command) => match command.to_ascii_lowercase().as_str() {
                "subscribe" => match command_args.pop_front() {
                    Some(topic_name) => {
                        self.subscribe(engine, topic_name);
                        Ok(None)
                    }
                    None => {
                        self.log_error("'subscribe' command requires topic name to subscribe to");
                        Ok(None)
                    }
                },
                "unsubscribe" => match command_args.pop_front() {
                    Some(topic_name) => {
                        self.unsubscribe(engine, topic_name);
                        Ok(None)
                    }
                    None => {
                        self.log_error(
                            "'unsubscribe' command requires topic name to unsubscribe from",
                        );
                        Ok(None)
                    }
                },
                "listen" => {
                    debug!("Got listen command");
                    match command_args.pop_front() {
                        Some(address) => {
                            self.listen(engine, address)?;
                        }
                        None => {
                            self.log_error(
                                "'listen' command requires network address to listen on",
                            );
                        }
                    }
                    Ok(None)
                }
                "connect" => {
                    debug!("Got connect command");
                    match command_args.pop_front() {
                        Some(address) => self.connect(engine, address)?,
                        None => {
                            self.log_error(
                                "'connect' command requires network address to connect to",
                            );
                        }
                    }
                    Ok(None)
                }
                "create-onion-service" => {
                    debug!("Got create-onion-service command");
                    match (command_args.pop_front(), command_args.pop_front()) {
                        (Some(virt_port_str), target_port_opt) => {
                            self.create_onion_service(engine, virt_port_str, target_port_opt)
                                .await;
                        }
                        _ => {
                            self.log_error("'create-onion-service' command requires at least one port argument");
                        }
                    };

                    Ok(None)
                }
                "findpeer" => {
                    debug!("Got findpeer command");
                    match command_args.pop_front() {
                        Some(peer_id) => self.find_peer(engine, peer_id).await?,
                        None => {
                            self.log_error("'findpeer' command requires Peer ID to find");
                        }
                    }
                    Ok(None)
                }
                "register" => {
                    debug!("Got register command");
                    match (command_args.pop_front(), command_args.pop_front()) {
                        (Some(namespace), Some(rendezvous_node)) => {
                            match self.register(engine, namespace, rendezvous_node) {
                                Ok(_) => {
                                    self.log_info(&format!(
                                        "Registered in namespace {}",
                                        namespace
                                    ));
                                }
                                Err(error) => {
                                    self.log_error(&format!("{}", error));
                                }
                            }
                        }
                        _ => {
                            self.log_error("'register' command requires a namespace and a rendezvous node peer ID");
                        }
                    }
                    Ok(None)
                }
                "discover" => {
                    debug!("Got register command");
                    match (
                        command_args.pop_front(),
                        command_args.pop_front(),
                        command_args.pop_front(),
                    ) {
                        (Some(rendezvous_node), namespace, limit) => {
                            let rendezvous_node = match PeerId::from_str(rendezvous_node) {
                                Ok(rendezvous_node) => rendezvous_node,
                                Err(error) => {
                                    self.log_error(&format!(
                                        "Error converting rendezvous node to PeerId: {}",
                                        error
                                    ));
                                    return Ok(None);
                                }
                            };
                            let namespace = match namespace {
                                Some(namespace) => match Namespace::new(namespace.to_string()) {
                                    Ok(namespace) => Some(namespace),
                                    Err(_) => {
                                        self.log_error("Namespace string is too long");
                                        return Ok(None);
                                    }
                                },
                                None => None,
                            };
                            let limit = match limit {
                                Some(limit) => match limit.parse::<u64>() {
                                    Ok(limit) => Some(limit),
                                    Err(error) => {
                                        self.log_error(&format!(
                                            "Error parsing limit parameter: {}",
                                            error,
                                        ));
                                        return Ok(None);
                                    }
                                },
                                None => None,
                            };
                            if let Err(error) =
                                engine.discover(namespace, None, limit, rendezvous_node)
                            {
                                self.log_error(&format!("Error in discover: {}", error));
                            }
                        }
                        _ => {
                            self.log_error("'discover' command requires a rendezvous node peer ID");
                        }
                    }
                    Ok(None)
                }
                "quit" => Ok(Some(InputEvent::Shutdown)),
                _ => {
                    self.log_error(&format!("Unknown command '{}'", command));
                    Ok(None)
                }
            },
            None => Ok(None),
        }
    }

    pub async fn handle_input_event(
        &mut self,
        engine: &mut Engine,
        event: Event,
    ) -> Result<Option<InputEvent>, Box<dyn std::error::Error>> {
        // debug!("Got input event {:?}", event);
        match event {
            Event::Mouse(_) => Ok(None),
            Event::Resize(_, _) => Ok(None),
            Event::Key(KeyEvent { code, modifiers }) => match code {
                KeyCode::Esc => Ok(Some(InputEvent::Shutdown)),
                KeyCode::Char(character) => {
                    if character == 'c' && modifiers.contains(KeyModifiers::CONTROL) {
                        Ok(Some(InputEvent::Shutdown))
                    } else if character == 'u' && modifiers.contains(KeyModifiers::CONTROL) {
                        self.clear_input_to_cursor();
                        Ok(None)
                    } else {
                        self.input_write(character);
                        Ok(None)
                    }
                }
                KeyCode::Enter => {
                    if let Some(input) = self.reset_input() {
                        if let Some(command) = input.strip_prefix('/') {
                            self.handle_command(engine, command.split_whitespace().collect())
                                .await
                        } else {
                            match self.subscription_list.current_index() {
                                Some(_) => {
                                    let topic = self.subscription_list.current().unwrap();
                                    match self.subscriptions.get_mut(topic) {
                                        Some(subscription) => {
                                            let topic = subscription.topic.clone();
                                            let message = ChatMessage::new(
                                                Some(self.my_identity),
                                                topic,
                                                input.clone(),
                                            );
                                            subscription.add_message(message);
                                            Ok(Some(InputEvent::Message {
                                                topic: IdentTopic::new(subscription.topic.clone()),
                                                message: input.into_bytes(),
                                            }))
                                        }
                                        None => {
                                            self.log_error("Not currently subscribed to anything");
                                            Ok(None)
                                        }
                                    }
                                }
                                None => {
                                    self.log_error("Not currently subscribed to anything");
                                    Ok(None)
                                }
                            }
                        }
                    } else {
                        Ok(None)
                    }
                }
                KeyCode::Delete => {
                    self.input_remove();
                    Ok(None)
                }
                KeyCode::Backspace => {
                    self.input_remove_previous();
                    Ok(None)
                }
                KeyCode::Left => {
                    if modifiers == KeyModifiers::CONTROL {
                        self.subscription_list.prev();
                    } else {
                        self.input_move_cursor(CursorMovement::Left);
                    }
                    Ok(None)
                }
                KeyCode::Right => {
                    if modifiers == KeyModifiers::CONTROL {
                        self.subscription_list.next();
                    } else {
                        self.input_move_cursor(CursorMovement::Right);
                    }
                    Ok(None)
                }
                KeyCode::Home => {
                    self.input_move_cursor(CursorMovement::Start);
                    Ok(None)
                }
                KeyCode::End => {
                    self.input_move_cursor(CursorMovement::End);
                    Ok(None)
                }
                KeyCode::Up => {
                    self.messages_scroll(ScrollMovement::Up);
                    Ok(None)
                }
                KeyCode::Down => {
                    self.messages_scroll(ScrollMovement::Down);
                    Ok(None)
                }
                KeyCode::PageUp => {
                    self.messages_scroll(ScrollMovement::Start);
                    Ok(None)
                }
                _ => Ok(None),
            },
        }
    }

    pub fn new_user(&mut self, peer_id: PeerId) {
        self.user_ids.insert(peer_id, self.last_user_id);
        self.last_user_id += 1;
    }

    pub fn get_user_id(&self, peer_id: &Option<PeerId>) -> Option<usize> {
        match peer_id {
            Some(peer_id) => self.user_ids.get(peer_id).cloned(),
            None => None,
        }
    }

    pub fn input_write(&mut self, character: char) {
        self.input.insert(self.input_cursor, character);
        self.input_cursor += 1;
    }

    pub fn input_remove(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input.remove(self.input_cursor);
        }
    }

    pub fn input_remove_previous(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            self.input.remove(self.input_cursor);
        }
    }

    pub fn input_move_cursor(&mut self, movement: CursorMovement) {
        match movement {
            CursorMovement::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                }
            }
            CursorMovement::Right => {
                if self.input_cursor < self.input.len() {
                    self.input_cursor += 1;
                }
            }
            CursorMovement::Start => {
                self.input_cursor = 0;
            }
            CursorMovement::End => {
                self.input_cursor = self.input.len();
            }
        }
    }

    pub fn messages_scroll(&mut self, movement: ScrollMovement) {
        match movement {
            ScrollMovement::Up => {
                if self.scroll_messages_view > 0 {
                    self.scroll_messages_view -= 1;
                }
            }
            ScrollMovement::Down => {
                self.scroll_messages_view += 1;
            }
            ScrollMovement::Start => {
                self.scroll_messages_view += 0;
            }
        }
    }

    pub fn clear_input_to_cursor(&mut self) {
        if !self.input.is_empty() {
            self.input.drain(..self.input_cursor);
            self.input_cursor = 0;
        }
    }

    pub fn reset_input(&mut self) -> Option<String> {
        if !self.input.is_empty() {
            self.input_cursor = 0;
            return Some(self.input.drain(..).collect());
        }
        None
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        match self.subscriptions.get_mut(&message.topic) {
            Some(subscription) => subscription.add_message(message),
            None => (),
        }
    }

    fn log_message(&mut self, level: Level, message: String) {
        self.log_messages.push(LogMessage {
            date: Local::now(),
            level,
            message,
        });
    }

    pub fn log_error(&mut self, message: &str) {
        self.log_message(Level::Error, format!("ERROR: {}", message));
    }

    pub fn log_warning(&mut self, message: &str) {
        self.log_message(Level::Warning, format!("WARNING: {}", message));
    }

    pub fn log_info(&mut self, message: &str) {
        self.log_message(Level::Info, format!("INFO: {}", message));
    }

    pub fn draw(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        // debug!("UI::draw called");
        match self.subscription_list.current() {
            Some(_) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(
                        [
                            Constraint::Length(1),
                            Constraint::Percentage(20),
                            Constraint::Length(3),
                            Constraint::Min(1),
                            Constraint::Length(1),
                            Constraint::Length(1),
                        ]
                        .as_ref(),
                    )
                    .split(chunk);

                self.draw_title_bar(frame, chunks[0]);
                self.draw_system_messages_panel(frame, chunks[1]);
                self.draw_chat_tabs(frame, chunks[2]);
                self.draw_chat_panel(frame, chunks[3]);
                self.draw_status_bar(frame, chunks[4]);
                self.draw_input_panel(frame, chunks[5]);
            }
            None => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(
                        [
                            Constraint::Length(1),
                            Constraint::Min(1),
                            Constraint::Length(1),
                            Constraint::Length(1),
                        ]
                        .as_ref(),
                    )
                    .split(chunk);

                self.draw_title_bar(frame, chunks[0]);
                self.draw_system_messages_panel(frame, chunks[1]);
                self.draw_status_bar(frame, chunks[2]);
                self.draw_input_panel(frame, chunks[3]);
            }
        }
    }

    fn draw_title_bar(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let title_bar = Paragraph::new(format!(
            "{} {}  |  Discovery methods: {}  |  Nat traversal methods: {}",
            crate_name!(),
            crate_version!(),
            self.discovery_methods,
            self.nat_traversal_methods,
        ))
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().bg(Color::Blue))
        .alignment(Alignment::Left);

        frame.render_widget(title_bar, chunk);
    }

    fn draw_system_messages_panel(
        &self,
        frame: &mut Frame<CrosstermBackend<impl Write>>,
        chunk: Rect,
    ) {
        let messages = self
            .log_messages
            .asc_iter()
            .map(|message| {
                let date = message.date.format("%H:%M:%S ").to_string();
                let color = match message.level {
                    Level::Info => Color::Gray,
                    Level::Warning => Color::Rgb(255, 127, 0),
                    Level::Error => Color::Red,
                };
                let ui_message = vec![
                    Span::styled(date, Style::default().fg(self.date_color)),
                    Span::styled(message.message.clone(), Style::default().fg(color)),
                ];
                Spans::from(ui_message)
            })
            .collect::<Vec<_>>();

        let messages_panel = Paragraph::new(messages)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                "System Messages",
                Style::default().add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().fg(self.chat_panel_color))
            .alignment(Alignment::Left)
            .scroll((self.scroll_messages_view() as u16, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(messages_panel, chunk);
    }

    fn draw_chat_tabs(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let tabs = Tabs::new(
            self.subscription_list
                .names()
                .iter()
                .map(|s| Spans::from(s.clone()))
                .collect(),
        )
        .block(Block::default().title("Chats").borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Yellow))
        .select(self.subscription_list.current_index().unwrap());

        frame.render_widget(tabs, chunk);
    }

    fn draw_chat_panel(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        match self.subscription_list.current() {
            Some(topic) => {
                let subscription = self.subscriptions.get(topic).unwrap();
                let messages = subscription
                    .messages
                    .asc_iter()
                    .map(|message| {
                        let color = match self.get_user_id(&message.user) {
                            Some(id) => self.message_colors[id % self.message_colors.len()],
                            None => self.my_user_color,
                        };
                        let date = message.date.format("%H:%M:%S ").to_string();
                        let long_username = message
                            .user
                            .map_or("<unknown>".to_string(), |u| u.to_base58());
                        let short_username = long_username[long_username.len() - 7..].to_string();
                        let mut ui_message = vec![
                            Span::styled(date, Style::default().fg(self.date_color)),
                            Span::styled(short_username, Style::default().fg(color)),
                            Span::styled(": ", Style::default().fg(color)),
                        ];
                        ui_message.extend(Self::parse_content(&message.message));
                        Spans::from(ui_message)
                    })
                    .collect::<Vec<_>>();

                let chat_panel = Paragraph::new(messages)
                    .block(Block::default().borders(Borders::ALL).title(Span::styled(
                        subscription.topic.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )))
                    .style(Style::default().fg(self.chat_panel_color))
                    .alignment(Alignment::Left)
                    .scroll((self.scroll_messages_view() as u16, 0))
                    .wrap(Wrap { trim: false });

                frame.render_widget(chat_panel, chunk);
            }
            None => (),
        }
    }

    fn draw_status_bar(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let status_bar = Paragraph::new("Input")
            .block(Block::default().borders(Borders::NONE))
            .style(Style::default().bg(Color::Blue))
            .alignment(Alignment::Left);

        frame.render_widget(status_bar, chunk);
    }

    fn parse_content(content: &str) -> Vec<Span> {
        vec![Span::raw(content)]
    }

    fn draw_input_panel(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let inner_width = (chunk.width - 2) as usize;

        let input = self.input.iter().collect::<String>();
        let input = split_each(input, inner_width)
            .into_iter()
            .map(|line| Spans::from(vec![Span::raw(line)]))
            .collect::<Vec<_>>();

        let input_panel = Paragraph::new(input)
            .block(Block::default().borders(Borders::NONE))
            .style(Style::default().fg(self.input_panel_color))
            .alignment(Alignment::Left);

        frame.render_widget(input_panel, chunk);

        let input_cursor = self.ui_input_cursor(inner_width);
        frame.set_cursor(chunk.x + input_cursor.0, chunk.y + input_cursor.1)
    }
}
