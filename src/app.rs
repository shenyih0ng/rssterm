use std::{
    cmp::{max, min},
    error::Error,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
    vec,
};

use chrono::DateTime;
use chrono_humanize::HumanTime;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    prelude::Backend,
    style::{Color, Stylize},
    text::{Line, Span},
    widgets::{
        Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        StatefulWidget, Table, TableState, Widget,
    },
};
use reqwest::Client;
use rss::{Channel, Item};
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
                    (_, KeyCode::Up | KeyCode::Char('k')) => self.feed.scroll(1, false),
                    (_, KeyCode::Down | KeyCode::Char('j')) => self.feed.scroll(1, true),
                    (_, KeyCode::Enter) => self.feed.open_selected(),
                    #[rustfmt::skip]
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => { self.should_quit = true;}
                    (KeyModifiers::SHIFT, KeyCode::Char('G')) => self.feed.scroll(u16::MAX, true),
                    _ => {}
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let [header_area, main_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Fill(1)])
            .areas(frame.area());

        let [title_area, time_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Length(30)])
            .flex(Flex::SpaceBetween)
            .areas(header_area);

        frame.render_widget(
            Paragraph::new(vec![Line::from(vec![
                Span::raw(env!("CARGO_PKG_NAME")).magenta().bold(),
                Span::raw(" "),
                Span::raw(env!("CARGO_PKG_VERSION")).dark_gray(),
            ])])
            .alignment(Alignment::Left),
            title_area,
        );

        frame.render_widget(
            Paragraph::new(chrono::Local::now().format("%H:%M:%S / %a %v").to_string())
                .dark_gray()
                .alignment(Alignment::Right),
            time_area,
        );

        frame.render_widget(&self.feed, main_area);
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
    row_heights_cum: Vec<usize>,
    tui_state: TableState,
}

#[derive(Clone, Default)]
struct FeedItem {
    title: String,
    url: String,
    pub_date: DateTime<chrono::Local>,
}

impl FeedItem {
    fn from_rss_item(item: &Item) -> Option<Self> {
        let pub_date = item.pub_date()?;
        // https://docs.rs/rss/2.0.12/rss/struct.Item.html#structfield.pub_date
        let parsed_date = DateTime::parse_from_rfc2822(pub_date).ok()?;
        Some(Self {
            title: item.title().unwrap_or("No Title 😢").to_string(),
            url: item.link().unwrap_or("No Link 😭").to_string(),
            pub_date: parsed_date.into(),
        })
    }
}

impl FeedWidget {
    const FEED_HIGHLIGHT_SYMBOL: &str = ">> ";
    const FEED_COLUMN_SPACING: u16 = 2;
    const HTTP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

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
                    let mut state = self.state.write().unwrap();
                    state
                        .data
                        .extend(rss_chan.items().iter().filter_map(FeedItem::from_rss_item));
                    state.data.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
                }
                Ok(Err(e)) => eprintln!("Feed fetch error: {}", e),
                Err(e) => eprintln!("Task failed: {}", e),
            }
        }
    }

    fn scroll(&self, delta: u16, is_down: bool) {
        let mut state = self.state.write().unwrap();
        if is_down {
            state.table.tui_state.scroll_down_by(delta);
        } else {
            state.table.tui_state.scroll_up_by(delta);
        }
        // NOTE: The range of selected_idx is [0, data.len() - 1]
        // This is likely to allow developers to catch overflow events to handle wrap arounds
        // Currently, we are not allowing wrap arounds, hence we are clamping the value
        let selected_idx = min(
            state.table.tui_state.selected().unwrap(),
            state.data.len().saturating_sub(1),
        );
        state.tui_scrollbar = state.tui_scrollbar.position(
            state
                .table
                .row_heights_cum
                .get(selected_idx.saturating_sub(1))
                .unwrap_or(&0)
                * min(selected_idx, 1),
        );
    }

    fn open_selected(&self) {
        let state = self.state.read().unwrap();
        match state.table.tui_state.selected() {
            Some(idx) => {
                let selected_idx = min(idx, state.data.len().saturating_sub(1));
                match state.data.get(selected_idx) {
                    Some(feed_item) => open::that(feed_item.url.clone())
                        .unwrap_or_else(|e| eprintln!("Failed to open URL: {}", e)),
                    None => eprintln!("No item selected"),
                }
            }
            _ => eprintln!("No item selected"),
        }
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
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(2)]).areas(area);

        let tui_table_col_layout = [Constraint::Fill(0), Constraint::Percentage(20)];
        let tui_table_hl_symbol_len = FeedWidget::FEED_HIGHLIGHT_SYMBOL.len() as u16;
        let tui_table_col_areas = Layout::horizontal(tui_table_col_layout).split(Rect {
            x: tui_table_area.x + tui_table_hl_symbol_len,
            width: tui_table_area
                .width
                .saturating_sub(tui_table_hl_symbol_len + FeedWidget::FEED_COLUMN_SPACING),
            ..tui_table_area
        });

        let mut state = self.state.write().unwrap();
        let data = state.data.clone();

        if state.table.tui_state.selected().is_none() && !data.is_empty() {
            state.table.tui_state.select(Some(0));
        }

        let mut tui_table_content_height = 0;
        let (tui_rows, tui_cum_row_heights): (Vec<Row>, Vec<usize>) = data
            .iter()
            .enumerate()
            .map(|(idx, feed_item)| {
                let (tui_row, tui_row_height) = feed_item.draw(&tui_table_col_areas);

                let is_last_row = idx == data.len().saturating_sub(1);
                let tui_row_btm_margin = (!is_last_row) as u16;

                let tui_row_total_height = tui_row_height + tui_row_btm_margin;
                tui_table_content_height += tui_row_total_height;

                (
                    tui_row.bottom_margin(tui_row_btm_margin),
                    tui_table_content_height as usize,
                )
            })
            .unzip();

        state.tui_scrollbar = state
            .tui_scrollbar
            .content_length(tui_table_content_height as usize);
        state.table.row_heights_cum = tui_cum_row_heights;

        let feed_table = Table::new(tui_rows, tui_table_col_layout)
            .highlight_symbol(Line::from(FeedWidget::FEED_HIGHLIGHT_SYMBOL).magenta())
            .highlight_spacing(HighlightSpacing::Always)
            .column_spacing(FeedWidget::FEED_COLUMN_SPACING);

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
    fn draw(&self, col_areas: &[Rect]) -> (Row<'_>, u16) {
        let wrapped_texts: Vec<Vec<String>> = col_areas
            .iter()
            .zip([
                self.title.clone(),
                HumanTime::from(self.pub_date).to_string(),
            ])
            .map(|(col_area, text)| {
                wrap(
                    &text,
                    Options::new(col_area.width as usize).break_words(true),
                )
                .into_iter()
                .map(|line| line.into_owned())
                .collect::<Vec<String>>()
            })
            .collect();

        let content_lines = [
            wrapped_texts[0]
                .iter()
                .map(|line| Line::from(line.clone()).white().bold())
                .collect::<Vec<_>>(),
            vec![Line::from(self.url.clone()).dark_gray()],
        ]
        .concat();

        let pub_date_lines = wrapped_texts[1]
            .iter()
            .map(|line| {
                Line::from(line.clone())
                    .light_blue()
                    .italic()
                    .alignment(Alignment::Right)
            })
            .collect::<Vec<_>>();

        let row_height = max(content_lines.len(), pub_date_lines.len()) as u16;
        (
            Row::new(vec![Cell::new(content_lines), Cell::new(pub_date_lines)]).height(row_height),
            row_height,
        )
    }
}
