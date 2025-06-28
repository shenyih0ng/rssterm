use clap::{Parser, Subcommand};
use ratatui::Terminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::CrosstermBackend;
use std::env::home_dir;
use std::error::Error;
use std::fs::{self};
use std::io::{Read, Write};
use std::panic::{set_hook, take_hook};
use std::path::PathBuf;
use std::time::Duration;
use std::{f32, io};
use url::Url;

mod app;
mod debug;
mod event;
mod stream;
mod utils;

use crate::app::App;

fn default_feeds_file() -> PathBuf {
    home_dir()
        .map(|home_dir| home_dir.join(".config/rssterm/feeds.txt"))
        // Fallback to relative path if HOME is not set
        .unwrap_or_else(|| PathBuf::from("feeds.txt"))
}

#[derive(Parser)]
#[command(version = env!("RSSTERM_VERSION"))]
#[command(about = "i read rss feeds on the terminal btw")]
struct Cli {
    #[arg(long = "feeds", env = "RSSTERM_FEEDS", default_value = default_feeds_file().into_os_string())]
    feeds_file: PathBuf,
    #[arg(
        long,
        default_value_t = 120.0,
        help = "Target rendering FPS (use 0 for uncapped)"
    )]
    fps: f32,
    #[arg(long, default_value_t = false)]
    show_fps: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Add a new RSS/Atom feed")]
    Add {
        #[arg(value_parser=Url::parse, help="URL of the RSS/Atom feed (e.g. https://hnrss.org/frontpage)")]
        url: Url,
    },
    #[command(about = "Path to feeds file")]
    Feeds,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Cli::parse();

    match args.command {
        Some(Commands::Feeds) => {
            println!("{}", args.feeds_file.display());
            return Ok(());
        }
        Some(Commands::Add { url }) => {
            let mut feeds_file = fs::OpenOptions::new()
                .read(true)
                .append(true)
                .open(args.feeds_file.clone())?;
            let mut feed_urls = String::new();
            feeds_file.read_to_string(&mut feed_urls)?;
            if feed_urls.lines().any(|line| line.trim() == url.as_str()) {
                eprintln!("{url} is already there!");
                return Ok(());
            }
            // Add a new line
            feeds_file.write(format!("\n{}", url).as_bytes())?;
            println!("Added feed: {}", url);
            return Ok(());
        }
        _ => {}
    }

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
        .run(&mut terminal, args.feeds_file, tick_rate, args.show_fps)
        .await?;

    term_restore()?;

    Ok(())
}
