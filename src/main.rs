use ratatui::Terminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::CrosstermBackend;
use std::env::VarError;
use std::error::Error;
use std::io;
use std::panic::{set_hook, take_hook};
use std::path::{Path, PathBuf};

mod app;
mod utils;

use crate::app::App;

const DEFAULT_FPS: f32 = 60.0;

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
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    fn term_restore() -> io::Result<()> {
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
        Ok(())
    }

    let default_panic_hook = take_hook();
    set_hook(Box::new(move |panic_info| {
        let _ = term_restore();
        default_panic_hook(panic_info);
    }));

    App::default()
        .run(&mut terminal, get_config_file()?, DEFAULT_FPS)
        .await?;

    term_restore()?;

    Ok(())
}
