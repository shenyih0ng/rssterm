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
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    prelude::Backend,
    style::{Color, Stylize},
    text::{Line, Span},
    widgets::{
        Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, TableState,
    },
};
use reqwest::Client;
use rss::{Channel, Item};
use tokio::{fs, task::JoinSet};
use tokio_stream::StreamExt;

use crate::utils::wrap_then_apply;

#[derive(Default)]
pub struct App {
    // app state
    should_quit: bool,
    // widgets
    feed: FeedWidget,
}

impl App {
    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
        config_file: PathBuf,
        fps: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.feed.run(
            fs::read_to_string(config_file)
                .await
                .map(|s| s.trim().lines().map(str::to_owned).collect())
                .unwrap_or(Vec::new()),
        );

        let mut tick_rate = tokio::time::interval(Duration::from_secs_f32(1.0 / fps));
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
                    (_, KeyCode::Up | KeyCode::Char('k')) => self.feed.scroll(-1),
                    (_, KeyCode::Down | KeyCode::Char('j')) => self.feed.scroll(1),
                    (_, KeyCode::Enter) => self.feed.open_selected(),
                    #[rustfmt::skip]
                        (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => { self.should_quit = true;}
                    (KeyModifiers::SHIFT, KeyCode::Char('G')) => self.feed.scroll(isize::MAX),
                    _ => {}
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
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

        self.feed.render(frame, main_area);
    }
}

#[derive(Clone)]
struct FeedWidget {
    data: Arc<RwLock<FeedWidgetData>>,
    http_client: Client,

    tb_state: TableState,
    // Cumulative rendered height of each row in the table
    tb_cum_row_heights: Vec<usize>,
    sb_state: ScrollbarState,
}

#[derive(Default)]
struct FeedWidgetData {
    items: Vec<FeedItem>,
}

impl Default for FeedWidget {
    fn default() -> Self {
        let http_client = Client::builder()
            .user_agent(Self::HTTP_USER_AGENT)
            .build()
            .expect("Failed to create HTTP client");
        Self {
            http_client,
            data: Arc::new(RwLock::new(FeedWidgetData::default())),
            tb_state: TableState::default(),
            tb_cum_row_heights: Vec::new(),
            sb_state: ScrollbarState::default(),
        }
    }
}

impl FeedWidget {
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
                    let mut data = self.data.write().unwrap();
                    data.items
                        .extend(rss_chan.items().iter().filter_map(FeedItem::from_rss_item));
                    data.items.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
                }
                Ok(Err(e)) => eprintln!("Feed fetch error: {}", e),
                Err(e) => eprintln!("Task failed: {}", e),
            }
        }
    }

    fn scroll(&mut self, delta: isize) {
        let abs_scroll_delta = delta.abs() as u16;
        if delta < 0 {
            self.tb_state.scroll_up_by(abs_scroll_delta);
        } else {
            self.tb_state.scroll_down_by(abs_scroll_delta);
        }
        // NOTE: The range of selected_idx is [0, data.len() - 1]
        // This is likely to allow developers to catch overflow events to handle wrap arounds
        // Currently, we are not allowing wrap arounds, hence we are clamping the value
        let selected_item_idx = self
            .tb_state
            .selected()
            .unwrap_or(0)
            .clamp(0, self.tb_cum_row_heights.len() - 1);
        // If the first item is selected, there should be no scrollbar movement (i.e. position 0)
        self.sb_state = self.sb_state.position(
            self.tb_cum_row_heights
                .get(selected_item_idx.saturating_sub(1))
                .unwrap_or(&0)
                * min(selected_item_idx, 1),
        );
    }

    fn open_selected(&self) {
        let data = self.data.read().unwrap();
        match self.tb_state.selected() {
            Some(selected_item_idx) => {
                if let Some(feed_item) = data.items.get(selected_item_idx) {
                    open::that(feed_item.url.clone())
                        .unwrap_or_else(|e| eprintln!("Failed to open URL: {}", e));
                } else {
                    eprintln!("No item selected");
                }
            }
            None => eprintln!("No item selected"),
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let [tb_area, sb_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(2)]).areas(area);

        let tb_col_spacing = 2;
        let tb_highlight_symbol = ">> ";
        // Dynamically calculate the rendered width of each table column, required for text wrapping
        let tb_col_layout = [Constraint::Fill(0), Constraint::Percentage(20)];
        let tb_hl_symbol_len = tb_highlight_symbol.len() as u16;
        let tb_col_areas = Layout::horizontal(tb_col_layout).split(Rect {
            x: tb_area.x + tb_hl_symbol_len,
            width: tb_area
                .width
                .saturating_sub(tb_hl_symbol_len + tb_col_spacing),
            ..tb_area
        });

        let feed_items = self.data.read().unwrap().items.clone();
        self.tb_cum_row_heights.resize(feed_items.len(), 0);

        let mut tbl_total_content_height = 0;
        let tb_rows: Vec<Row> = feed_items
            .iter()
            .enumerate()
            .map(|(idx, feed_item)| {
                let (tb_row, tb_row_h) = feed_item.draw(&tb_col_areas);
                let tb_row_btm_margin = (!(idx == feed_items.len().saturating_sub(1))) as u16;
                let tb_row_total_h = tb_row_h + tb_row_btm_margin;
                tbl_total_content_height += tb_row_total_h as usize;
                // Each row has a dynamic height based on text wrapping therefore, cumulative row heights are updated every render cycle
                self.tb_cum_row_heights[idx] = tbl_total_content_height;
                tb_row.bottom_margin(tb_row_btm_margin)
            })
            .collect();

        self.sb_state = self.sb_state.content_length(tbl_total_content_height);

        // If there are not currently selected item and there are items in the feed, select the first item
        if self.tb_state.selected().is_none() && !feed_items.is_empty() {
            self.tb_state.select(Some(0));
        }

        let table = Table::new(tb_rows, tb_col_layout)
            .highlight_symbol(Line::from(tb_highlight_symbol).magenta())
            .highlight_spacing(HighlightSpacing::Always)
            .column_spacing(tb_col_spacing);

        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐")
            .thumb_style(Color::DarkGray);

        frame.render_stateful_widget(table, tb_area, &mut self.tb_state);
        frame.render_stateful_widget(scrollbar, sb_area, &mut self.sb_state);
    }
}

impl FeedItem {
    fn draw(&self, col_areas: &[Rect]) -> (Row<'_>, u16) {
        let w_title = wrap_then_apply(&self.title, col_areas[0].width as usize, |line| {
            Line::from(line).white().bold()
        });
        let content_lines = [w_title, vec![Line::from(self.url.clone()).dark_gray()]].concat();

        let w_pub_date = wrap_then_apply(
            &HumanTime::from(self.pub_date).to_string(),
            col_areas[1].width as usize,
            |line| {
                Line::from(line)
                    .light_blue()
                    .italic()
                    .alignment(Alignment::Right)
            },
        );

        let row_height = max(content_lines.len(), w_pub_date.len()) as u16;
        (
            Row::new(vec![Cell::new(content_lines), Cell::new(w_pub_date)]).height(row_height),
            row_height,
        )
    }
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
