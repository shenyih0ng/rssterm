use clap::Parser;
use ratatui::Terminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::CrosstermBackend;
use std::env::VarError;
use std::error::Error;
use std::panic::{set_hook, take_hook};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{f32, io};

mod app;
mod debug;
mod event;
mod utils;

use crate::app::App;

#[derive(Parser)]
#[command(version)]
#[command(about = "i read rss feeds on the terminal btw")]
struct Cli {
    #[arg(
        long,
        default_value_t = 60.0,
        help = "Target rendering FPS (use 0 for uncapped)"
    )]
    fps: f32,
    #[arg(long, default_value_t = false)]
    show_fps: bool,
}

// TODO: Let user specify the config file path via CLI argument
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
    let args = Cli::parse();

    let tick_rate = if args.fps == 0.0 {
        Duration::from_secs_f32(f32::EPSILON)
    } else {
        Duration::from_secs_f32(1.0 / args.fps)
    };

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
        .run(&mut terminal, get_config_file()?, tick_rate, args.show_fps)
        .await?;

    term_restore()?;

    Ok(())
}
