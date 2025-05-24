use std::{
    cmp::{max, min},
    error::Error,
    hash::{DefaultHasher, Hash, Hasher},
    iter::once,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
    vec,
};

use chrono::DateTime;
use chrono_humanize::HumanTime;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use itertools::{chain, intersperse};
use ratatui::{
    Frame, Terminal,
    layout::{Flex, Layout, Rect, Size},
    prelude::{Backend, StatefulWidget},
    style::{Color, Stylize},
    text::Line,
    widgets::{
        Block, BorderType, HighlightSpacing, Padding, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Widget, Wrap,
    },
};
use ratatui_macros::{constraints, horizontal, line, row, span, vertical};
use reqwest::Client;
use rss::{Channel, Item};
use tokio::{fs, sync::mpsc::Receiver, sync::mpsc::Sender, task::JoinSet};
use tokio_stream::StreamExt;
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use crate::{
    event::AppEvent,
    para_wrap,
    utils::{parse_html_or, wrap_then_apply},
};

use crate::debug::FpsWidget;

pub struct App {
    // app state
    should_quit: bool,
    // widgets
    feed: FeedWidget,
    // perf/debug widgets
    fps: Option<FpsWidget>,

    app_event_rx: Receiver<AppEvent>,
}

impl Default for App {
    fn default() -> Self {
        let (app_event_tx, app_event_rx) = tokio::sync::mpsc::channel(1);
        Self {
            should_quit: false,
            feed: FeedWidget::new(app_event_tx.clone()),
            fps: None,
            app_event_rx,
        }
    }
}

impl App {
    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
        config_file: PathBuf,
        tick_rate: Duration,
        show_fps: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if show_fps {
            self.fps = Some(FpsWidget::default());
        }

        self.feed.run(
            fs::read_to_string(config_file)
                .await
                .map(|s| s.trim().lines().map(str::to_owned).collect())
                .unwrap_or(Vec::new()),
        );

        let mut tick_rate = tokio::time::interval(tick_rate);
        let mut term_events = EventStream::new();

        while !self.should_quit {
            tokio::select! {
                _ = tick_rate.tick() => { terminal.draw(|frame| self.draw(frame))?; }
                Some(Ok(term_event)) = term_events.next() => self.handle_term_event(&term_event).await,
                Some(AppEvent::Exit) = self.app_event_rx.recv() => self.should_quit = true
            }
        }

        Ok(())
    }

    async fn handle_term_event(&mut self, event: &Event) {
        let app_event = match event {
            Event::Key(key) => self.parse_term_key_event(key),
            _ => None,
        };

        if let Some(app_event) = app_event {
            match app_event {
                AppEvent::Exit => self.should_quit = true,
                // Since there is only one active widget (`FeedWidget`), we can directly dispatch all
                // non-exit events to it. When more widgets are added, we will need to identify which
                // widget is active and dispatch the event accordingly.
                _ => self.feed.handle_event(app_event).await,
            }
        }
    }

    // Map terminal (crossterm) key events to app events
    // Can be thought of as the key binding handler
    fn parse_term_key_event(&self, key_event: &KeyEvent) -> Option<AppEvent> {
        // We are only interested in key press events
        if key_event.kind != KeyEventKind::Press {
            return None;
        }
        match (key_event.modifiers, key_event.code) {
            (_, KeyCode::Up | KeyCode::Char('k')) => Some(AppEvent::Scroll(-1)),
            (_, KeyCode::Down | KeyCode::Char('j')) => Some(AppEvent::Scroll(1)),
            (_, KeyCode::Char('g')) => Some(AppEvent::Scroll(isize::MIN)),
            (KeyModifiers::SHIFT, KeyCode::Char('G')) => Some(AppEvent::Scroll(isize::MAX)),

            (_, KeyCode::Enter) => Some(AppEvent::Expand),
            (_, KeyCode::Char('q')) => Some(AppEvent::Close),

            (_, KeyCode::Char('o')) => Some(AppEvent::Open),

            (KeyModifiers::CONTROL, KeyCode::Char('c')) => Some(AppEvent::Exit),
            _ => None,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [header_area, main_area, fps_area] =
            vertical![==2, *=1, ==(self.fps.is_some() as u16)].areas(frame.area());

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

        if let Some(fps_widget) = &mut self.fps {
            fps_widget.render(fps_area, frame.buffer_mut());
        }
    }
}

#[derive(Clone)]
struct FeedWidget {
    app_event_tx: Sender<AppEvent>,

    data: Arc<RwLock<FeedWidgetData>>,
    http_client: Client,

    tb_state: TableState,
    // Cumulative rendered height of each row in the table
    tb_cum_row_heights: Vec<usize>,
    sb_state: ScrollbarState,

    exp_item_id: Option<u64>,
    exp_item_render_state: ScrollViewState,
}

#[derive(Default)]
struct FeedWidgetData {
    items: Vec<FeedItem>,
}

impl FeedWidget {
    const HTTP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

    fn new(app_event_tx: Sender<AppEvent>) -> Self {
        let http_client = Client::builder()
            .user_agent(Self::HTTP_USER_AGENT)
            .build()
            .expect("Failed to create HTTP client");
        Self {
            app_event_tx,
            http_client,
            data: Arc::new(RwLock::new(FeedWidgetData::default())),
            tb_state: TableState::default(),
            tb_cum_row_heights: Vec::new(),
            sb_state: ScrollbarState::default(),
            exp_item_id: None,
            exp_item_render_state: ScrollViewState::default(),
        }
    }

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

    async fn handle_event(&mut self, event: AppEvent) {
        let is_exp_item_active = self.exp_item_id.is_some();
        match event {
            AppEvent::Scroll(delta) => {
                if is_exp_item_active {
                    self.scroll_exp_item(delta);
                } else {
                    self.scroll_feed(delta);
                }
            }
            AppEvent::Expand => {
                let items = &self.data.read().unwrap().items;
                if let Some(selected_item_idx) = self.tb_state.selected() {
                    if let Some(feed_item) = items.get(selected_item_idx) {
                        self.exp_item_id = Some(feed_item.id);
                    }
                }
            }
            AppEvent::Close => {
                if self.exp_item_id.is_some() {
                    self.exp_item_id = None;
                    // Reset the scroll view state so that it does not persist between items
                    self.exp_item_render_state = ScrollViewState::default();
                } else {
                    // If the feed widget does not have a nested view that can be closed,
                    // we send a exit event upstream. We can do this because if a widget
                    // receives an event, it is the only active/focused widget of the
                    // entire application, as such the widget can safely determine whether
                    // to exit the application
                    self.app_event_tx.send(AppEvent::Exit).await.ok();
                }
            }
            AppEvent::Open => self.open_selected(),
            _ => (),
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
        let items = &self.data.read().unwrap().items;

        let open_result = self
            .tb_state
            .selected()
            .and_then(|idx| items.get(idx))
            .and_then(|item| item.url.as_ref())
            .map(|url| open::that(url));

        match open_result {
            Some(Err(e)) => eprintln!("Failed to open URL: {}", e),
            None => eprintln!("No item selected or no URL available"),
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let feed_items = &self.data.read().unwrap().items;

        if let Some(exp_feed_item) = self
            .exp_item_id
            .and_then(|id| feed_items.iter().find(|item| item.id == id))
        {
            exp_feed_item.render(frame, area, &mut self.exp_item_render_state);
            return;
        }

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

        // Select the expanded item if available, otherwise select first item if none selected
        let selected_item_index = self
            .exp_item_id
            .and_then(|item_id| feed_items.iter().position(|item| item.id == item_id))
            .or_else(|| match self.tb_state.selected() {
                None if !feed_items.is_empty() => Some(0),
                current => current,
            });
        self.tb_state.select(selected_item_index);

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
    fn draw_row(&self, col_areas: &[Rect; 2]) -> (Row<'_>, u16) {
        let [label_width, pub_date_width] = col_areas.map(|area| area.width);

        let w_title = {
            let title_width = label_width as usize;
            match &self.title {
                Some(title_text) => wrap_then_apply(&title_text, title_width, |line_str| {
                    line!(line_str).white().bold()
                }),
                None => wrap_then_apply(&"untitled".to_string(), title_width, |line_str| {
                    line!(line_str).dark_gray().bold()
                }),
            }
        };

        let content_lines = match self.url {
            Some(ref url) => chain(w_title, vec![line!(url).dark_gray()]).collect(),
            None => w_title,
        };

        let w_pub_date = wrap_then_apply(
            &HumanTime::from(self.pub_date).to_string(),
            pub_date_width as usize,
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
        let title_lines = match &self.title {
            Some(title_text) => wrap_then_apply(title_text, render_area.width as usize, |line| {
                line!(line).white().bold()
            }),
            None => vec![line!("untitled").dark_gray().bold()],
        };

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
            .as_deref()
            .or(self.description.as_deref())
            .unwrap_or("");

        let sv_total_width = content_area.width;
        // -3: padding between the content and the scrollbar
        let sv_content_width = content_area.width - 3;

        let wrapped_content =
            wrap_then_apply(content, sv_content_width as usize, |line| line!(line));
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
    id: u64,
    title: Option<String>,
    url: Option<String>,
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

        // Generate a unique ID for the item
        let mut hasher = DefaultHasher::default();
        item.title.hash(&mut hasher);
        item.description.hash(&mut hasher);
        item.pub_date.hash(&mut hasher);

        Some(Self {
            id: hasher.finish(),
            title: item.title().map(str::to_string),
            url: item.link().map(str::to_string),
            // https://docs.rs/rss/2.0.12/rss/struct.Item.html#structfield.pub_date
            pub_date: DateTime::parse_from_rfc2822(item.pub_date()?).ok()?.into(),
            authors,
            description,
            content,
        })
    }
}
