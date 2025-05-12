use std::{
    error::Error,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use chrono::DateTime;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    prelude::Backend,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Cell, HighlightSpacing, Paragraph, Row, StatefulWidget, Table, TableState, Widget},
};
use reqwest::Client;
use rss::Channel;
use tokio::{fs, task::JoinSet};
use tokio_stream::StreamExt;

#[derive(Default)]
pub struct App {
    // app state
    should_quit: bool,
    // widgets
    feed: FeedWidget,
}

impl App {
    const NAME: &str = "rssterm";
    const FRAMES_PER_SECOND: f32 = 60.0;

    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
        config_file: PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.feed.run(
            fs::read_to_string(config_file)
                .await
                .map(|s| s.trim().lines().map(str::to_owned).collect())
                .unwrap_or(Vec::new()),
        );

        let mut tick_rate =
            tokio::time::interval(Duration::from_secs_f32(1.0 / Self::FRAMES_PER_SECOND));
        let mut events = EventStream::new();

        while !self.should_quit {
            tokio::select! {
                _ = tick_rate.tick() => { terminal.draw(|frame| self.draw(frame))?; }
                Some(Ok(event)) = events.next() => self.handle_event(&event),
            }
        }

        Ok(())
    }

    fn handle_event(&mut self, event: &Event) {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => {
                        self.should_quit = true;
                    }
                    (_, KeyCode::Up | KeyCode::Char('k')) => self.feed.scroll_up(),
                    (_, KeyCode::Down | KeyCode::Char('j')) => self.feed.scroll_down(),
                    (KeyModifiers::SHIFT, KeyCode::Char('G')) => self.feed.scroll_to_bottom(),
                    _ => {}
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Fill(1)])
            .split(frame.area());

        let header_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Length(30)])
            .flex(Flex::SpaceBetween)
            .split(chunks[0]);

        frame.render_widget(
            Paragraph::new(vec![Line::from(vec![
                Span::styled(Self::NAME, Style::default().magenta()),
                Span::raw(" "),
                Span::styled(env!("CARGO_PKG_VERSION"), Style::default().dark_gray()),
            ])])
            .alignment(Alignment::Left),
            header_layout[0],
        );

        frame.render_widget(
            Paragraph::new(chrono::Local::now().format("%H:%M:%S / %a %v").to_string())
                .dark_gray()
                .alignment(Alignment::Right),
            header_layout[1],
        );
        frame.render_widget(&self.feed, chunks[1]);
    }
}

#[derive(Clone)]
struct FeedWidget {
    http_client: Client,
    state: Arc<RwLock<FeedState>>,
}

#[derive(Default)]
struct FeedState {
    items: Vec<FeedItem>,
    table_state: TableState,
}

struct FeedItem {
    title: String,
    url: String,
    pub_date: DateTime<chrono::Local>,
}

impl FeedWidget {
    const HTTP_USER_AGENT: &str = "i read rss feeds on the terminal btw"; // iusevimbtw.com

    fn run(&self, chan_urls: Vec<String>) {
        tokio::spawn(self.clone().fetch_feed(chan_urls));
    }

    async fn fetch_feed(self, chan_urls: Vec<String>) {
        let mut query_set: JoinSet<Result<Channel, Box<dyn Error + Send + Sync>>> = JoinSet::new();

        for chan_url in chan_urls {
            let local_http_client = self.http_client.clone();
            query_set.spawn(async move {
                let http_resp = local_http_client.get(chan_url).send().await?;
                return Ok(Channel::read_from(&(http_resp.bytes().await?)[..])?);
            });
        }

        let mut rss_items = Vec::new();
        while let Some(result) = query_set.join_next().await {
            match result {
                Ok(Ok(rss_chan)) => {
                    rss_items.extend(rss_chan.items().into_iter().map(|item| {
                        FeedItem {
                            title: item.title().unwrap_or("No Title 😢").to_string(),
                            url: item.link().unwrap_or("No Link 😭").to_string(),
                            pub_date: DateTime::parse_from_rfc2822(item.pub_date().unwrap())
                                .unwrap()
                                .into(),
                        }
                    }));
                }
                Ok(Err(e)) => eprintln!("Feed fetch error: {}", e),
                Err(e) => eprintln!("Task failed: {}", e),
            }
        }

        let mut feed_state = self.state.write().unwrap();
        feed_state.table_state = TableState::default();
        if !rss_items.is_empty() {
            feed_state.table_state.select(Some(0));
        }
        feed_state.items = rss_items;
    }

    fn scroll_down(&self) {
        self.state.write().unwrap().table_state.scroll_down_by(1);
    }

    fn scroll_up(&self) {
        self.state.write().unwrap().table_state.scroll_up_by(1);
    }

    fn scroll_to_bottom(&self) {
        self.state.write().unwrap().table_state.select_last();
    }
}

impl Default for FeedWidget {
    fn default() -> Self {
        let http_client = Client::builder()
            .user_agent(Self::HTTP_USER_AGENT)
            .build()
            .expect("Failed to create HTTP client");
        Self {
            http_client,
            state: Arc::new(RwLock::new(FeedState::default())),
        }
    }
}

impl Widget for &FeedWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut state = self.state.write().unwrap();

        let table = Table::new(&state.items, [Constraint::Fill(1)])
            .highlight_symbol(Line::from(">> ").style(Style::default().magenta()))
            .highlight_spacing(HighlightSpacing::Always);

        StatefulWidget::render(table, area, buf, &mut state.table_state);
    }
}

impl From<&FeedItem> for Row<'_> {
    fn from(item: &FeedItem) -> Self {
        Row::new(vec![Cell::new(vec![
            Line::from(Span::styled(
                item.title.clone(),
                Style::default().blue().bold(),
            )),
            Line::from(Span::styled(item.url.clone(), Style::default().gray())),
            Line::from(Span::styled(
                item.pub_date.format("%-d-%b-%Y").to_string(),
                Style::default().dark_gray(),
            )),
        ])])
        .height(4)
    }
}
