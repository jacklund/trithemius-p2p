use crate::{config::Theme, state::Window};
use log::{debug, error, LevelFilter};
use resize::Type::Lanczos3;
use resize::{px::RGB, Pixel::RGB8};

use super::state::State;
use super::util::split_each;

use tui::backend::CrosstermBackend;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Modifier, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, Paragraph, Wrap};
use tui::Frame;

use std::io::Write;

pub fn draw(
    frame: &mut Frame<CrosstermBackend<impl Write>>,
    state: &State,
    chunk: Rect,
    theme: &Theme,
) {
    debug!("UI::draw called");
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(6)].as_ref())
        .split(chunk);

    let upper_chunk = chunks[0];
    if !state.windows.is_empty() {
        let upper_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(30)].as_ref())
            .split(upper_chunk);
        draw_messages_panel(frame, state, upper_chunks[0], theme);
        draw_video_panel(frame, state, upper_chunks[1]);
    } else {
        draw_messages_panel(frame, state, chunks[0], theme);
    }
    draw_input_panel(frame, state, chunks[1], theme);
}

fn draw_messages_panel(
    frame: &mut Frame<CrosstermBackend<impl Write>>,
    state: &State,
    chunk: Rect,
    theme: &Theme,
) {
    let message_colors = &theme.message_colors;

    let messages = state
        .messages()
        .iter()
        .rev()
        .map(|message| {
            let color = match state.get_user_id(&message.user) {
                Some(id) => message_colors[id % message_colors.len()],
                None => theme.my_user_color,
            };
            let date = message.date.format("%H:%M:%S ").to_string();
            let mut ui_message = vec![
                Span::styled(date, Style::default().fg(theme.date_color)),
                Span::styled(message.user.to_base58(), Style::default().fg(color)),
                Span::styled(": ", Style::default().fg(color)),
            ];
            ui_message.extend(parse_content(&message.message, theme));
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
        .scroll((state.scroll_messages_view() as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(messages_panel, chunk);
}

fn parse_content<'a>(content: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    vec![Span::raw(content)]
}

fn draw_input_panel(
    frame: &mut Frame<CrosstermBackend<impl Write>>,
    state: &State,
    chunk: Rect,
    theme: &Theme,
) {
    let inner_width = (chunk.width - 2) as usize;

    let input = state.input().iter().collect::<String>();
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

    let input_cursor = state.ui_input_cursor(inner_width);
    frame.set_cursor(chunk.x + 1 + input_cursor.0, chunk.y + 1 + input_cursor.1)
}

fn draw_video_panel(frame: &mut Frame<CrosstermBackend<impl Write>>, state: &State, chunk: Rect) {
    let windows = state.windows.values().collect();
    let fb = FrameBuffer::new(windows).block(Block::default().borders(Borders::ALL));
    frame.render_widget(fb, chunk);
}
#[derive(Default)]
struct FrameBuffer<'a> {
    windows: Vec<&'a Window>,
    block: Option<Block<'a>>,
}

impl<'a> FrameBuffer<'a> {
    fn new(windows: Vec<&'a Window>) -> Self {
        Self {
            windows,
            ..Default::default()
        }
    }

    fn block(mut self, block: Block<'a>) -> FrameBuffer<'a> {
        self.block = Some(block);
        self
    }
}

impl tui::widgets::Widget for FrameBuffer<'_> {
    fn render(mut self, area: Rect, buf: &mut tui::buffer::Buffer) {
        let area = match self.block.take() {
            Some(b) => {
                let inner_area = b.inner(area);
                b.render(area, buf);
                inner_area
            }
            None => area,
        };

        let windows_num = self.windows.len();
        let window_height = area.height / windows_num as u16;
        let y_start = area.y;
        for (idx, window) in self.windows.iter().enumerate() {
            let area = Rect::new(
                area.x,
                y_start + window_height * idx as u16,
                area.width,
                window_height,
            );

            let mut resizer = resize::new(
                window.width / 2,
                window.height,
                area.width as usize,
                area.height as usize,
                RGB8,
                Lanczos3,
            )
            .unwrap();
            let mut dst = vec![RGB::new(0, 0, 0); (area.width * area.height) as usize];
            resizer.resize(&window.data, &mut dst).unwrap();

            let mut dst = dst.iter();
            for j in area.y..area.y + area.height {
                for i in area.x..area.x + area.width {
                    let rgb = dst.next().unwrap();
                    let r = rgb.r;
                    let g = rgb.g;
                    let b = rgb.b;
                    buf.get_mut(i, j).set_bg(Color::Rgb(r, g, b));
                }
            }
        }
    }
}
