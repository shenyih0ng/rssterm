use ratatui::Terminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::CrosstermBackend;
use std::env::VarError;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};

mod app;

use crate::app::App;

fn get_config_file() -> Result<PathBuf, VarError> {
    let config_file = if let Ok(env_file_path) = std::env::var("RSSTERM_CONFIG") {
        PathBuf::from(env_file_path)
    } else {
        std::env::var("HOME").map(|home_dir| Path::new(&home_dir).join(".rssterm.config"))?
    };
    Ok(config_file)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // set up terminal
    enable_raw_mode()?;
    let mut io_stream = io::stdout();
    execute!(io_stream, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io_stream);
    let mut terminal = Terminal::new(backend)?;

    App::default()
        .run(&mut terminal, get_config_file()?)
        .await?;

    // restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
