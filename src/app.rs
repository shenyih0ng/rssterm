use std::{
    cmp::{max, min},
    error::Error,
    iter::once,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
    vec,
};

use chrono::DateTime;
use chrono_humanize::HumanTime;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use itertools::intersperse;
use ratatui::{
    Frame, Terminal,
    layout::{Flex, Layout, Margin, Rect, Size},
    prelude::{Backend, StatefulWidget},
    style::{Color, Stylize},
    text::Line,
    widgets::{
        Block, BorderType, Clear, HighlightSpacing, Padding, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Widget, Wrap,
    },
};
use ratatui_macros::{constraints, horizontal, line, row, span, vertical};
use reqwest::Client;
use rss::{Channel, Item};
use tokio::{fs, task::JoinSet};
use tokio_stream::StreamExt;
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use crate::{
    event::AppEvent,
    para_wrap,
    utils::{parse_html_or, wrap_then_apply},
};

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
                // Since there is only one active widget (`FeedWidget`), we can directly dispatch all
                // non-quit events to it. When more widgets are added, we will need to identify which
                // widget is active and dispatch the event accordingly.
                match (key.modifiers, key.code) {
                    #[rustfmt::skip]
                    (_, KeyCode::Up | KeyCode::Char('k')) => self.feed.handle_event(AppEvent::Scroll(-1)),
                    #[rustfmt::skip]
                    (_, KeyCode::Down | KeyCode::Char('j')) => self.feed.handle_event(AppEvent::Scroll(1)),
                    (_, KeyCode::Char('g')) => self.feed.handle_event(AppEvent::Scroll(isize::MIN)),
                    #[rustfmt::skip]
                    (KeyModifiers::SHIFT, KeyCode::Char('G')) => self.feed.handle_event(AppEvent::Scroll(isize::MAX)),
                    (_, KeyCode::Enter) => self.feed.handle_event(AppEvent::Open),
                    (_, KeyCode::Char('o')) => self.feed.handle_event(AppEvent::Expand),
                    (_, KeyCode::Char('q') | KeyCode::Esc) => {
                        self.feed.handle_event(AppEvent::Collapse)
                    }
                    #[rustfmt::skip]
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) => { self.should_quit = true;}
                    _ => {}
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [header_area, main_area] = vertical![==2, *=1].areas(frame.area());
        let [title_area, time_area] = horizontal![==30, ==30]
            .flex(Flex::SpaceBetween)
            .areas(header_area);

        frame.render_widget(
            Paragraph::new(line![
                span!(env!("CARGO_PKG_NAME")).magenta().bold(),
                span!(" "),
                span!(env!("CARGO_PKG_VERSION")).dark_gray(),
            ])
            .left_aligned(),
            title_area,
        );

        frame.render_widget(
            Paragraph::new(
                chrono::Local::now()
                    .format("%H:%M:%S / %e-%b-%Y [%a]")
                    .to_string(),
            )
            .dark_gray()
            .right_aligned(),
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

    exp_item_idx: Option<usize>,
    exp_item_render_state: ScrollViewState,
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
            exp_item_idx: None,
            exp_item_render_state: ScrollViewState::default(),
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

    fn handle_event(&mut self, event: AppEvent) {
        let is_exp_item_active = self.exp_item_idx.is_some();
        match event {
            AppEvent::Scroll(delta) => {
                if is_exp_item_active {
                    self.scroll_exp_item(delta);
                } else {
                    self.scroll_feed(delta);
                }
            }
            AppEvent::Expand => self.exp_item_idx = self.tb_state.selected(),
            AppEvent::Collapse => {
                self.exp_item_idx = None;
                // Reset the scroll view state so that it does not persist between items
                self.exp_item_render_state = ScrollViewState::default();
            }
            AppEvent::Open => self.open_selected(),
        }
    }

    fn scroll_feed(&mut self, delta: isize) {
        if delta == isize::MAX || delta == isize::MIN {
            if delta < 0 {
                self.tb_state.select_first();
            } else {
                self.tb_state.select_last();
            }
        } else {
            let abs_scroll_delta = delta.abs() as u16;
            if delta < 0 {
                self.tb_state.scroll_up_by(abs_scroll_delta);
            } else {
                self.tb_state.scroll_down_by(abs_scroll_delta);
            }
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

    fn scroll_exp_item(&mut self, delta: isize) {
        if delta == isize::MAX || delta == isize::MIN {
            if delta < 0 {
                self.exp_item_render_state.scroll_to_top();
            } else {
                self.exp_item_render_state.scroll_to_bottom();
            }
        } else {
            (0..delta.abs()).for_each(|_| {
                if delta < 0 {
                    self.exp_item_render_state.scroll_up();
                } else {
                    self.exp_item_render_state.scroll_down();
                }
            });
        }
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
        let [tb_area, sb_area] = horizontal![*=1, ==2].areas(area);

        let tb_col_spacing = 2;
        let tb_highlight_symbol = ">> ";
        // Dynamically calculate the rendered width of each table column, required for text wrapping
        let tb_col_layout = constraints![*=0, ==20%];
        let tb_hl_symbol_len = tb_highlight_symbol.len() as u16;
        let tb_col_areas: [Rect; 2] = Layout::horizontal(tb_col_layout).areas(Rect {
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
                let (tb_row, tb_row_h) = feed_item.draw_row(&tb_col_areas);
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

        if self.exp_item_idx.is_some() {
            let popup_area = area.inner(Margin::new(area.width / 16, area.height / 16));
            Clear.render(popup_area, frame.buffer_mut());
            feed_items[self.exp_item_idx.unwrap()].render(
                frame,
                popup_area,
                &mut self.exp_item_render_state,
            );
        }
    }
}

impl FeedItem {
    fn draw_row(&self, col_areas: &[Rect; 2]) -> (Row<'_>, u16) {
        let w_title = wrap_then_apply(&self.title, col_areas[0].width as usize, |line| {
            line!(line).white().bold()
        });
        let content_lines = [w_title, vec![line!(self.url.clone()).dark_gray()]].concat();

        let w_pub_date = wrap_then_apply(
            &HumanTime::from(self.pub_date).to_string(),
            col_areas[1].width as usize,
            |line| line!(line).light_blue().italic().right_aligned(),
        );

        let row_height = max(content_lines.len(), w_pub_date.len()) as u16;
        (
            row![content_lines, w_pub_date].height(row_height),
            row_height,
        )
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &mut ScrollViewState) {
        // The enclosing area that renders as an outline for the popup
        let outline_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Color::LightMagenta)
            .padding(Padding::symmetric(2, 1));

        let render_area = outline_block.inner(area);
        // Dynamically wrap the title to calculate height required for full visibility
        // `Paragraph::wrap` is not enough to guarantee visibility if the allocated
        // area is smaller than the wrapped text. Therefore, we will need to dynamically
        // set the height of the render area for the title
        let title_lines = wrap_then_apply(&self.title, render_area.width as usize, |line| {
            line!(line).white().bold()
        });
        let title_h = title_lines.len() as u16;
        // Assume that the metadata (authors and pub_date) will only ever take up 2 lines
        // This is not the best as there will be a breaking point where parts of metadata will be
        // hidden if the width of the terminal is too small
        let meta_h: u16 = 2;

        let [header_area, _, content_area, _]: [Rect; 4] =
            // +1: padding between title and metadata
            vertical![==(title_h + meta_h + 1), ==1, *=0, ==1].areas(render_area);

        let [title_area, _, meta_area]: [Rect; 3] =
            vertical![==title_h, ==1, ==meta_h].areas(header_area);

        let [authors_area, pub_date_area]: [Rect; 2] = horizontal![==50%, ==50%]
            .flex(Flex::SpaceBetween)
            .areas(meta_area);

        frame.render_widget(outline_block, area);
        frame.render_widget(Paragraph::new(title_lines), title_area);

        if !self.authors.is_empty() {
            let authors_line = Line::from(
                once(span!("by ").dark_gray())
                    .chain(intersperse(
                        self.authors
                            .iter()
                            .map(|author| span!(author).light_green().italic()),
                        span!(", ").dark_gray(),
                    ))
                    .collect::<Vec<_>>(),
            );
            frame.render_widget(para_wrap!(authors_line), authors_area);
        }

        let pub_date_para =
            para_wrap!(self.pub_date.format("%H:%M:%S / %e-%b-%Y [%a]").to_string())
                .light_blue()
                .italic()
                .right_aligned();
        frame.render_widget(pub_date_para, pub_date_area);

        let content = self
            .content
            .clone()
            .unwrap_or_else(|| self.description.clone().unwrap_or_default());

        let sv_total_width = content_area.width;
        // -3: padding between the content and the scrollbar
        let sv_content_width = content_area.width - 3;

        let wrapped_content =
            wrap_then_apply(&content, sv_content_width as usize, |line| line!(line));
        let sv_total_height = wrapped_content.len() as u16;

        let mut content_sv = ScrollView::new(Size {
            width: sv_total_width,
            height: sv_total_height,
        })
        .horizontal_scrollbar_visibility(ScrollbarVisibility::Never);
        // NOTE: This area is relative to the scrollview, not the frame
        let sv_content_area = Rect {
            width: sv_content_width,
            height: sv_total_height,
            ..Rect::ZERO
        };

        content_sv.render_widget(Paragraph::new(wrapped_content), sv_content_area);
        content_sv.render(content_area, frame.buffer_mut(), state);
    }
}

#[derive(Clone, Default)]
struct FeedItem {
    title: String,
    url: String,
    authors: Vec<String>,
    description: Option<String>,
    content: Option<String>,
    pub_date: DateTime<chrono::Local>,
}

impl FeedItem {
    fn from_rss_item(item: &Item) -> Option<Self> {
        let mut authors = match item.dublin_core_ext {
            Some(ref dcmi_ext) => dcmi_ext
                .creators()
                .iter()
                .map(|creator| str::to_string(creator))
                .collect(),
            None => Vec::new(),
        };
        // Prioritise dublin core metadata over RSS metadata
        // This is just a guess, but it seems like the dublin core metadata is more reliable and
        // more widely used based on the feeds I am subscribed to
        if authors.is_empty() {
            item.author().map(|author| authors.push(author.to_string()));
        }

        let description = item
            .description()
            .map(|desc| parse_html_or(desc, desc.to_string()));

        let content = item
            .content()
            .map(|content| parse_html_or(content, content.to_string()));

        Some(Self {
            title: item.title().unwrap_or("No Title 😢").to_string(),
            url: item.link().unwrap_or("No Link 😭").to_string(),
            // https://docs.rs/rss/2.0.12/rss/struct.Item.html#structfield.pub_date
            pub_date: DateTime::parse_from_rfc2822(item.pub_date()?).ok()?.into(),
            authors,
            description,
            content,
        })
    }
}
