use chrono::{DateTime, Local};
use clap::{crate_name, crate_version};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use crossterm::ExecutableCommand;
use libp2p::gossipsub::IdentTopic;
use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};
use log::debug;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use trithemiuslib::{
    engine_event::EngineEvent,
    tor::{auth::TorAuthentication, control_connection::TorControlConnection},
    ChatMessage, Engine, InputEvent,
};

use tui::backend::CrosstermBackend;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::Color;
use tui::style::{Modifier, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, Paragraph, Wrap};
use tui::Frame;
use tui::Terminal;

use std::collections::{HashMap, VecDeque};
use std::io::Write;

enum Level {
    Info,
    Warning,
    Error,
}

enum Message {
    LogMessage {
        date: DateTime<Local>,
        level: Level,
        message: String,
    },
    ChatMessage(ChatMessage),
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
        terminal::enable_raw_mode().unwrap();
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

pub struct UI {
    my_identity: PeerId,
    messages: Vec<Message>,
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
}

impl UI {
    pub fn new(my_identity: PeerId) -> Self {
        Self {
            my_identity,
            messages: Vec::new(),
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
        }
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

    fn parse_u16<'a>(value: Option<&'a str>) -> Result<Option<u16>, std::num::ParseIntError> {
        value.and_then(|s| Some(s.parse::<u16>())).transpose()
    }

    fn parse_network_address<'a>(
        value: Option<&'a str>,
    ) -> Result<Option<Multiaddr>, Box<dyn std::error::Error>> {
        match value {
            None => Ok(None),
            Some(addr) => {
                if addr.starts_with("/") {
                    Ok(Some(Multiaddr::from_str(addr)?))
                } else {
                    let mut multiaddr = Multiaddr::empty();
                    let parts = addr.split(":").collect::<Vec<&str>>();
                    multiaddr.push(if let Ok(ipv4) = Ipv4Addr::from_str(parts[0]) {
                        Protocol::Ip4(ipv4)
                    } else if let Ok(ipv6) = Ipv6Addr::from_str(parts[0]) {
                        Protocol::Ip6(ipv6)
                    } else {
                        Protocol::Dns(parts[0].into())
                    });
                    match Self::parse_u16(Some(parts[1])) {
                        Ok(Some(port)) => {
                            multiaddr.push(Protocol::Tcp(port));
                            Ok(Some(multiaddr))
                        }
                        Ok(None) => Ok(None),
                        Err(error) => Err(error)?,
                    }
                }
            }
        }
    }

    async fn handle_command<'a>(
        &mut self,
        engine: &mut Engine,
        mut command_args: VecDeque<&'a str>,
    ) -> Result<Option<InputEvent>, std::io::Error> {
        let command = command_args.pop_front();
        match command {
            Some(command) => match command.to_ascii_lowercase().as_str() {
                "subscribe" => match command_args.pop_front() {
                    Some(topic_name) => {
                        match engine.subscribe(topic_name) {
                            Ok(true) => {
                                self.log_info(&format!("Subscribed to topic '{}'", topic_name))
                            }
                            Ok(false) => self
                                .log_info(&format!("Already subscribed to topic '{}'", topic_name)),
                            Err(error) => self.log_error(&format!(
                                "Error subscribing to topic {}: {}",
                                topic_name, error
                            )),
                        };
                        Ok(None)
                    }
                    None => {
                        self.log_error("'subscribe' command requires topic name to subscribe to");
                        Ok(None)
                    }
                },
                "listen" => {
                    debug!("Got listen command");
                    match Self::parse_network_address(command_args.pop_front()) {
                        Ok(Some(network_addr)) => {
                            engine.listen(network_addr).unwrap();
                        }
                        Ok(None) => {
                            self.log_error("Network address {} not parsable");
                        }
                        Err(error) => {
                            self.log_error(&format!("Error parsing network address: {}", error));
                        }
                    }
                    Ok(None)
                }
                "connect" => {
                    debug!("Got connect command");
                    match Self::parse_network_address(command_args.pop_front()) {
                        Ok(Some(network_addr)) => {
                            engine.dial(network_addr).unwrap();
                        }
                        Ok(None) => {
                            self.log_error("Network address {} not parsable");
                        }
                        Err(error) => {
                            self.log_error(&format!("Error parsing network address: {}", error));
                        }
                    }
                    Ok(None)
                }
                "create-onion-service" => {
                    debug!("Got create-onion-service command");
                    let (virt_port, target_port) = match Self::parse_u16(command_args.pop_front()) {
                        Ok(Some(virt_port)) => match Self::parse_u16(command_args.pop_front()) {
                            Ok(Some(target_port)) => {
                                debug!(
                                    "Got virt_port = {}, target_port = {}",
                                    virt_port, target_port
                                );
                                (virt_port, target_port)
                            }
                            Ok(None) => {
                                debug!(
                                    "Got virt_port = {}, target_port = {}",
                                    virt_port, virt_port
                                );
                                (virt_port, virt_port)
                            }
                            Err(error) => {
                                debug!("Got error parsing virt_port: {}", error);
                                self.log_error(&format!("Error parsing target_port: {}", error));
                                return Ok(None);
                            }
                        },
                        Ok(None) => {
                            self.log_error("Must specify a virtual port for create-onion-service");
                            return Ok(None);
                        }
                        Err(error) => {
                            self.log_error(&format!("Error parsing virtual port: {}", error));
                            return Ok(None);
                        }
                    };

                    match engine
                        .create_transient_onion_service(virt_port, target_port)
                        .await
                    {
                        Ok(onion_service) => {
                            self.log_info(&format!(
                                "Created onion service {}.onion:{}",
                                onion_service.service_id, onion_service.virt_port
                            ));
                        }
                        Err(error) => {
                            self.log_error(&format!("Error creating onion service: {}", error));
                        }
                    }

                    Ok(None)
                }
                "unsubscribe" => match command_args.pop_front() {
                    Some(topic_name) => {
                        match engine.unsubscribe(topic_name) {
                            Ok(true) => {
                                self.log_info(&format!("Unsubscribed to topic '{}'", topic_name))
                            }
                            Ok(false) => self
                                .log_info(&format!("Never subscribed to topic '{}'", topic_name)),
                            Err(error) => self.log_error(&format!(
                                "Error unsubscribing to topic {}: {}",
                                topic_name, error
                            )),
                        };
                        Ok(None)
                    }
                    None => {
                        self.log_error(
                            "'unsubscribe' command requires topic name to unsubscribe from",
                        );
                        Ok(None)
                    }
                },
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
    ) -> Result<Option<InputEvent>, std::io::Error> {
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
                            let message = ChatMessage::new(self.my_identity, input.clone());
                            self.add_message(message);
                            Ok(Some(InputEvent::Message {
                                topic: IdentTopic::new("chat"), // TODO: Change this
                                message: input.into_bytes(),
                            }))
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
                    self.input_move_cursor(CursorMovement::Left);
                    Ok(None)
                }
                KeyCode::Right => {
                    self.input_move_cursor(CursorMovement::Right);
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

    pub fn get_user_id(&self, peer_id: &PeerId) -> Option<usize> {
        self.user_ids.get(peer_id).cloned()
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
        self.messages.push(Message::ChatMessage(message));
    }

    fn log_message(&mut self, level: Level, message: String) {
        self.messages.push(Message::LogMessage {
            date: Local::now(),
            level,
            message: message,
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
        self.draw_messages_panel(frame, chunks[1]);
        self.draw_status_bar(frame, chunks[2]);
        self.draw_input_panel(frame, chunks[3]);
    }

    fn draw_title_bar(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let title_bar = Paragraph::new(format!("{} {}", crate_name!(), crate_version!()))
            .block(Block::default().borders(Borders::NONE))
            .style(Style::default().bg(Color::Blue))
            .alignment(Alignment::Left);

        frame.render_widget(title_bar, chunk);
    }

    fn draw_messages_panel(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let messages = self
            .messages
            .iter()
            .map(|message| match message {
                Message::ChatMessage(message) => {
                    let color = match self.get_user_id(&message.user) {
                        Some(id) => self.message_colors[id % self.message_colors.len()],
                        None => self.my_user_color,
                    };
                    let date = message.date.format("%H:%M:%S ").to_string();
                    let long_username = message.user.to_base58();
                    let short_username = long_username[long_username.len() - 7..].to_string();
                    let mut ui_message = vec![
                        Span::styled(date, Style::default().fg(self.date_color)),
                        Span::styled(short_username, Style::default().fg(color)),
                        Span::styled(": ", Style::default().fg(color)),
                    ];
                    ui_message.extend(Self::parse_content(&message.message));
                    Spans::from(ui_message)
                }
                Message::LogMessage {
                    date,
                    level,
                    message,
                } => {
                    let date = date.format("%H:%M:%S ").to_string();
                    let color = match level {
                        Level::Info => Color::Gray,
                        Level::Warning => Color::Rgb(255, 127, 0),
                        Level::Error => Color::Red,
                    };
                    let ui_message = vec![
                        Span::styled(date, Style::default().fg(self.date_color)),
                        Span::styled(message, Style::default().fg(color)),
                    ];
                    Spans::from(ui_message)
                }
            })
            .collect::<Vec<_>>();

        let messages_panel = Paragraph::new(messages)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                "LAN Room",
                Style::default().add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().fg(self.chat_panel_color))
            .alignment(Alignment::Left)
            .scroll((self.scroll_messages_view() as u16, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(messages_panel, chunk);
    }

    fn draw_status_bar(&self, frame: &mut Frame<CrosstermBackend<impl Write>>, chunk: Rect) {
        let status_bar = Paragraph::new("something")
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
