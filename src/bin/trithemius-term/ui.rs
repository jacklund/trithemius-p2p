use crate::config::Theme;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use crossterm::ExecutableCommand;
use libp2p::PeerId;
use log::debug;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use trithemiuslib::{ChatMessage, EngineEvent};

use tui::backend::CrosstermBackend;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Modifier, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, Paragraph, Wrap};
use tui::Frame;
use tui::Terminal;

use std::collections::HashMap;
use std::io::Write;

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

    pub fn render(&mut self, ui: &UI, theme: &Theme) -> Result<(), std::io::Error> {
        self.terminal
            .draw(|frame| ui.draw(frame, frame.size(), theme))?;
        Ok(())
    }
}

pub struct UI {
    my_identity: PeerId,
    messages: Vec<ChatMessage>,
    scroll_messages_view: usize,
    input: Vec<char>,
    input_cursor: usize,
    user_ids: HashMap<PeerId, usize>,
    last_user_id: usize,
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

    pub fn handle_input_event(
        &mut self,
        event: Event,
    ) -> Result<Option<EngineEvent>, std::io::Error> {
        match event {
            Event::Mouse(_) => Ok(None),
            Event::Resize(_, _) => Ok(None),
            Event::Key(KeyEvent { code, modifiers }) => match code {
                KeyCode::Esc => Ok(Some(EngineEvent::Shutdown)),
                KeyCode::Char(character) => {
                    if character == 'c' && modifiers.contains(KeyModifiers::CONTROL) {
                        Ok(Some(EngineEvent::Shutdown))
                    } else {
                        self.input_write(character);
                        Ok(None)
                    }
                }
                KeyCode::Enter => {
                    if let Some(input) = self.reset_input() {
                        let message = ChatMessage::new(self.my_identity, input.clone());
                        self.add_message(message.clone());
                        Ok(Some(EngineEvent::ChatMessage(message)))
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

    pub fn reset_input(&mut self) -> Option<String> {
        if !self.input.is_empty() {
            self.input_cursor = 0;
            return Some(self.input.drain(..).collect());
        }
        None
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    pub fn draw(
        &self,
        frame: &mut Frame<CrosstermBackend<impl Write>>,
        chunk: Rect,
        theme: &Theme,
    ) {
        debug!("UI::draw called");
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(6)].as_ref())
            .split(chunk);

        let upper_chunk = chunks[0];
        self.draw_messages_panel(frame, chunks[0], theme);
        self.draw_input_panel(frame, chunks[1], theme);
    }

    fn draw_messages_panel(
        &self,
        frame: &mut Frame<CrosstermBackend<impl Write>>,
        chunk: Rect,
        theme: &Theme,
    ) {
        let message_colors = &theme.message_colors;

        let messages = self
            .messages
            .iter()
            .rev()
            .map(|message| {
                let color = match self.get_user_id(&message.user) {
                    Some(id) => message_colors[id % message_colors.len()],
                    None => theme.my_user_color,
                };
                let date = message.date.format("%H:%M:%S ").to_string();
                let mut ui_message = vec![
                    Span::styled(date, Style::default().fg(theme.date_color)),
                    Span::styled(message.user.to_base58(), Style::default().fg(color)),
                    Span::styled(": ", Style::default().fg(color)),
                ];
                ui_message.extend(Self::parse_content(&message.message, theme));
                Spans::from(ui_message)
            })
            .collect::<Vec<_>>();

        let messages_panel = Paragraph::new(messages)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                "LAN Room",
                Style::default().add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().fg(theme.chat_panel_color))
            .alignment(Alignment::Left)
            .scroll((self.scroll_messages_view() as u16, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(messages_panel, chunk);
    }

    fn parse_content<'a>(content: &'a str, theme: &Theme) -> Vec<Span<'a>> {
        vec![Span::raw(content)]
    }

    fn draw_input_panel(
        &self,
        frame: &mut Frame<CrosstermBackend<impl Write>>,
        chunk: Rect,
        theme: &Theme,
    ) {
        let inner_width = (chunk.width - 2) as usize;

        let input = self.input.iter().collect::<String>();
        let input = split_each(input, inner_width)
            .into_iter()
            .map(|line| Spans::from(vec![Span::raw(line)]))
            .collect::<Vec<_>>();

        let input_panel = Paragraph::new(input)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                "Your message",
                Style::default().add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().fg(theme.input_panel_color))
            .alignment(Alignment::Left);

        frame.render_widget(input_panel, chunk);

        let input_cursor = self.ui_input_cursor(inner_width);
        frame.set_cursor(chunk.x + 1 + input_cursor.0, chunk.y + 1 + input_cursor.1)
    }
}
