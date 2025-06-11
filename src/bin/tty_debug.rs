use crossterm::event::{Event, EventStream, KeyCode, KeyEvent};
use rand::prelude::*;
use std::{
    fs::File,
    io::{self, Write, stdout},
    os::fd::AsRawFd,
    time::Duration,
};
use termios::*;
use tokio_stream::StreamExt;

const WORK_SYMBOL: char = '_';
const MISC_EVENT_SYMBOL: char = '.';
const EXPECTED_EVENT_SYMBOL: char = 'E';
const UNEXPECTED_EVENT_SYMBOL: char = 'X';

const COLOR_DIM: &str = "\x1b[2m";
const COLOR_RED_BOLD: &str = "\x1b[31;1m";
const COLOR_MAGENTA: &str = "\x1b[35;1m";
const COLOR_RESET: &str = "\x1b[0m";

fn is_expected_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Up | KeyCode::Down,
            ..
        })
    )
}

fn is_unexpected_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            ..
        })
    )
}

async fn run_event_explorer() {
    let mut reader = EventStream::new();
    let mut interval = tokio::time::interval(Duration::from_millis(1));

    let mut rng = rand::rng();
    let mut pause_render = false;

    let mut show_full_event_info = 0;

    loop {
        tokio::select! {
            // Simulate rendering/processing workload
            _ = interval.tick() => {
                if !pause_render { print!("{COLOR_DIM}{WORK_SYMBOL}{COLOR_RESET}"); }
                // Assume that rendering a frame takes 1s/(200-300fps)
                let dur_ms = rng.random_range(1.0e3/300.0..=1.0e3/200.0);
                tokio::time::sleep(Duration::from_millis(dur_ms as u64)).await;
            },
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        if event == Event::Key(KeyCode::Char('q').into()) {
                            break;
                        } else if event == Event::Key(KeyCode::Char('p').into()) {
                            pause_render = !pause_render;
                        } else if pause_render {
                            continue
                        } else if show_full_event_info > 0 {
                            let color = if is_expected_event(&event) {
                                COLOR_DIM
                            } else if is_unexpected_event(&event) {
                                COLOR_RED_BOLD
                            } else {
                                COLOR_MAGENTA
                            };
                            print!("{color}{:?}{COLOR_RESET}\r\n", event);

                            show_full_event_info -= 1;
                            if show_full_event_info == 0 { print!("\n"); }
                        } else if is_expected_event(&event) {
                            print!("{COLOR_DIM}{EXPECTED_EVENT_SYMBOL}{COLOR_RESET}");
                        } else if is_unexpected_event(&event) {
                            print!("{COLOR_RED_BOLD}{UNEXPECTED_EVENT_SYMBOL}{COLOR_RESET}");
                            // When an unexpected event is received, show the full event info for the next 5 events.
                            show_full_event_info = 5;
                            print!("\r\n\n{COLOR_RED_BOLD}{:?}{COLOR_RESET}\r\n", event);
                        } else {
                            print!("{COLOR_DIM}{MISC_EVENT_SYMBOL}{COLOR_RESET}");
                        }
                    },
                    Some(Err(e)) => println!("Error: {:?}\r", e),
                    None => break,
                }
            },
        }
        stdout().flush().unwrap();
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut tty = File::options().read(true).write(true).open("/dev/tty")?;
    let tty_fd = tty.as_raw_fd();

    let mut termios = Termios::from_fd(tty_fd)?;
    let og_termios = termios.clone();

    // Enter raw mode
    cfmakeraw(&mut termios);
    tcsetattr(tty_fd, TCSANOW, &termios)?;

    // Enter alternative screen buffer
    tty.write("\x1b[?1049h".as_bytes())?;
    // Move the cursor to the top (left) of the screen
    tty.write("\x1b[H".as_bytes())?;

    run_event_explorer().await;

    // Exit alternative screen buffer
    tty.write("\x1b[?1049l".as_bytes())?;
    // Restore original terminal settings
    tcsetattr(tty_fd, TCSANOW, &og_termios)?;

    Ok(())
}
