use crate::config::Theme;
use crate::state::State;
use crate::ui::{self};
use crate::util::Result;

use crossterm::terminal::{self};
use crossterm::ExecutableCommand;
use log::{debug, error, LevelFilter};

use tui::backend::CrosstermBackend;
use tui::Terminal;

pub struct Renderer {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl Renderer {
    pub fn new() -> Self {
        debug!("New renderer");
        terminal::enable_raw_mode().unwrap();
        let mut out = std::io::stdout();
        out.execute(terminal::EnterAlternateScreen).unwrap();

        Renderer {
            terminal: Terminal::new(CrosstermBackend::new(out)).unwrap(),
        }
    }

    pub fn render(&mut self, state: &State, theme: &Theme) -> Result<()> {
        debug!("Called Renderer::render");
        self.terminal
            .draw(|frame| ui::draw(frame, state, frame.size(), theme))?;
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.terminal
            .backend_mut()
            .execute(terminal::LeaveAlternateScreen)
            .expect("Could not execute to stdout");
        terminal::disable_raw_mode().expect("Terminal doesn't support to disable raw mode");
        if std::thread::panicking() {
            eprintln!(
                "termchat paniced, to log the error you can redirect stderror to a file, example: termchat 2> termchat_log",
            );
        }
    }
}
