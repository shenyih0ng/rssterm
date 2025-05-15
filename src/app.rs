use std::{
    error::Error,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
    usize::MAX,
    vec,
};

use chrono::DateTime;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    prelude::Backend,
    style::{Color, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        StatefulWidget, Table, TableState, Widget,
    },
};
use reqwest::Client;
use rss::Channel;
use textwrap::{Options, wrap};
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
                Span::raw(Self::NAME).magenta().bold(),
                Span::raw(" "),
                Span::raw(env!("CARGO_PKG_VERSION")).dark_gray(),
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
    data: Vec<FeedItem>,
    table: FeedTableState,
    tui_scrollbar: ScrollbarState,
}

#[derive(Default)]
struct FeedTableState {
    row_heights: Vec<usize>, // TODO should be a cumulative sum?
    tui_state: TableState,
}

#[derive(Clone, Default)]
struct FeedItem {
    title: String,
    url: String,
    pub_date: DateTime<chrono::Local>,
}

impl FeedWidget {
    const FEED_HIGHLIGHT_SYMBOL: &str = ">> ";
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

        while let Some(result) = query_set.join_next().await {
            match result {
                Ok(Ok(rss_chan)) => {
                    self.state
                        .write()
                        .unwrap()
                        .data
                        .extend(rss_chan.items().into_iter().map(|item| {
                            FeedItem {
                                title: item.title().unwrap_or("No Title 😢").to_string(),
                                url: item.link().unwrap_or("No Link 😭").to_string(),
                                // https://docs.rs/rss/2.0.12/rss/struct.Item.html#structfield.pub_date
                                pub_date: DateTime::parse_from_rfc2822(item.pub_date().unwrap())
                                    .unwrap()
                                    .into(),
                            }
                        }))
                }
                Ok(Err(e)) => eprintln!("Feed fetch error: {}", e),
                Err(e) => eprintln!("Task failed: {}", e),
            }
        }
    }

    fn scroll_down(&self) {
        let mut state = self.state.write().unwrap();
        state.table.tui_state.scroll_down_by(1);
        let selected_idx = state.table.tui_state.selected().unwrap();
        state.tui_scrollbar = state.tui_scrollbar.position(
            state
                .table
                .row_heights
                .iter()
                .take(selected_idx + 1)
                .sum::<usize>(),
        );
    }

    fn scroll_up(&self) {
        let mut state = self.state.write().unwrap();
        state.table.tui_state.scroll_up_by(1);
        let selected_idx = state.table.tui_state.selected().unwrap();
        state.tui_scrollbar = state.tui_scrollbar.position(
            state
                .table
                .row_heights
                .iter()
                .take(selected_idx)
                .sum::<usize>(),
        );
    }

    fn scroll_to_bottom(&self) {
        let mut state = self.state.write().unwrap();
        state.table.tui_state.select_last();
        state.tui_scrollbar = state.tui_scrollbar.position(MAX);
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
        let [tui_table_area, tui_scrollbar_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(3)]).areas(area);

        let tui_table_col_layout = [Constraint::Fill(0)];
        let tui_table_hl_symbol_len = FeedWidget::FEED_HIGHLIGHT_SYMBOL.len() as u16;
        let tui_table_col_areas = Layout::horizontal(tui_table_col_layout).split(Rect {
            x: tui_table_area.x + tui_table_hl_symbol_len,
            width: tui_table_area.width.saturating_sub(tui_table_hl_symbol_len),
            ..tui_table_area
        });

        let mut state = self.state.write().unwrap();
        let data = state.data.clone();

        if state.table.tui_state.selected().is_none() && !data.is_empty() {
            state.table.tui_state.select(Some(0));
        }

        let mut tui_table_content_height = 0;
        let (tui_rows, tui_row_heights): (Vec<Row>, Vec<usize>) = data
            .iter()
            .enumerate()
            .map(|(idx, feed_item)| {
                let tui_text = feed_item.draw(Some(tui_table_col_areas[0].width as usize));
                let tui_text_height = tui_text.height();

                let is_last_row = idx == data.len().saturating_sub(1);
                let tui_row_btm_margin = (!is_last_row) as usize;

                let tui_row_total_height = tui_text_height + tui_row_btm_margin;
                tui_table_content_height += tui_row_total_height;

                (
                    Row::new(vec![Cell::new(tui_text)])
                        .height(tui_text_height as u16)
                        .bottom_margin(tui_row_btm_margin as u16),
                    tui_row_total_height,
                )
            })
            .unzip();

        state.tui_scrollbar = state
            .tui_scrollbar
            .content_length(tui_table_content_height as usize);
        state.table.row_heights = tui_row_heights;

        let feed_table = Table::new(tui_rows, tui_table_col_layout)
            .highlight_symbol(Line::from(FeedWidget::FEED_HIGHLIGHT_SYMBOL).magenta())
            .highlight_spacing(HighlightSpacing::Always);

        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐")
            .thumb_style(Color::DarkGray);

        StatefulWidget::render(feed_table, tui_table_area, buf, &mut state.table.tui_state);
        StatefulWidget::render(scrollbar, tui_scrollbar_area, buf, &mut state.tui_scrollbar);
    }
}

impl FeedItem {
    fn draw(&self, width: Option<usize>) -> Text<'_> {
        let title_lines = wrap(
            &self.title,
            Options::new(width.unwrap_or(usize::MAX)).break_words(true),
        )
        .iter()
        .map(|line| Line::from(line.to_string()).blue().bold())
        .collect::<Vec<_>>();

        let mut lines = title_lines;
        lines.extend(vec![
            Line::from(self.url.clone()).gray(),
            Line::from(self.pub_date.format("%-d-%b-%Y").to_string()).dark_gray(),
        ]);

        lines.into()
    }
}
