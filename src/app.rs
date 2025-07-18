use std::{
    borrow::Cow,
    cmp::{max, min},
    error::Error,
    hash::{DefaultHasher, Hash, Hasher},
    num::{NonZero, NonZeroU64},
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
    vec,
};

use chrono::DateTime;
use chrono_humanize::HumanTime;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use itertools::chain;
use ratatui::{
    Frame, Terminal,
    layout::{Flex, Layout, Margin, Rect},
    prelude::Backend,
    style::{Color, Stylize},
    text::{Line, Text},
    widgets::{
        Block, BorderType, HighlightSpacing, Padding, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState, Widget,
    },
};
use ratatui_macros::{constraints, horizontal, line, row, span, text, vertical};
use reqwest::Client;
use tokio::{
    fs,
    sync::mpsc::{Receiver, Sender},
    task::JoinSet,
};
use tokio_stream::StreamExt;
use url::Url;

use crate::{
    event::AppEvent,
    para_wrap,
    stream::RateLimitedEventStream,
    utils::{LONG_TIMESTAMP_FMT, Throbber, WARM_WHITE_RGB, try_parse_html, wrap_then_apply},
};

use crate::debug::FpsWidget;

pub struct App {
    // app state
    should_quit: bool,
    // widgets
    throbber: Throbber,
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
            throbber: Throbber::new(Duration::from_millis(250)),
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
        feeds_file: PathBuf,
        tick_rate: Duration,
        show_fps: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if show_fps {
            self.fps = Some(FpsWidget::default());
        }

        let feed_urls = fs::read_to_string(feeds_file)
            .await
            .map(|content| {
                content
                    .lines()
                    .map(str::trim)
                    .filter_map(|line| {
                        if !line.is_empty() {
                            Url::parse(line).ok().map(|url| url.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        self.feed.run(feed_urls);

        let mut tick_rate = tokio::time::interval(tick_rate);

        /*
         Currently, only scroll events (up/down/mouse scroll) are rate-limited to 15ms.
         The logic for determining whether an event should be rate-limited is in the `RateLimitedEventStream`.

         Delay of 15ms maintains smooth scrolling (1s/15ms = 66.67 FPS) while preventing event flooding
         from high-sensitivity mice (e.g. MX Master's fast scroll wheel).
        */
        let mut term_events = RateLimitedEventStream::new(Duration::from_millis(15));

        while !self.should_quit {
            tokio::select! {
                biased;
                Some(Ok(term_event)) = term_events.next() => self.handle_term_event(&term_event).await,
                Some(AppEvent::Exit) = self.app_event_rx.recv() => self.should_quit = true,
                _ = tick_rate.tick() => { terminal.draw(|frame| self.draw(frame))?; }
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

    // Map terminal (crossterm) key events to app event - can be thought of as the key binding handler
    fn parse_term_key_event(&mut self, key_event: &KeyEvent) -> Option<AppEvent> {
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

            (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(AppEvent::Exit),
            _ => None,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let fps_widget_h = if self.fps.is_some() { 1 } else { 0 };
        let [header_area, main_area, _, footer_area, _, fps_area] =
            vertical![==2, *=1, ==1, ==1, ==fps_widget_h, ==fps_widget_h]
                .areas(frame.area().inner(Margin::new(1, 1)));

        let [h_left_area, h_right_area] = horizontal![==1/2, ==1/2].areas(header_area);

        let app_name = env!("CARGO_PKG_NAME");
        let app_version = format!("v{}", env!("RSSTERM_VERSION"));
        let title_len = (app_name.len() + app_version.len() + 1) as u16; // +1 for space

        let [title_area, _, throbber_area] = horizontal![==title_len, ==1, ==1].areas(h_left_area);

        if self.feed.is_loading() {
            let tui_throbber = throbber_widgets_tui::Throbber::default()
                .throbber_set(throbber_widgets_tui::CANADIAN);
            self.throbber
                .render(tui_throbber, throbber_area, frame.buffer_mut());
        }

        frame.render_widget(
            line![
                span!(app_name).magenta().bold(),
                span!(" "),
                span!(app_version).blue()
            ]
            .left_aligned(),
            title_area,
        );

        frame.render_widget(
            line!(chrono::Local::now().format(LONG_TIMESTAMP_FMT).to_string())
                .cyan()
                .right_aligned(),
            h_right_area,
        );

        self.feed.render(frame, main_area);

        let help_key_desc = [
            ("j/k/↑/↓", "scroll"),
            ("g/G", "top/btm"),
            ("Enter", "expand"),
            ("o", "open"),
            ("q", "close"),
            ("Ctrl+D", "exit"),
        ];

        let mut help_spans = vec![];
        for (i, (key, desc)) in help_key_desc.iter().enumerate() {
            if i > 0 {
                help_spans.push(span!(" | "));
            }
            help_spans.extend(vec![span!(key).bold(), span!(" {}", desc)]);
        }
        frame.render_widget(
            // Custom fixed colour to ensure readability (against dark themed terminals)
            Line::from(help_spans).fg(Color::Rgb(100, 116, 139)),
            footer_area,
        );

        if let Some(fps_widget) = &mut self.fps {
            fps_widget.render(fps_area, frame.buffer_mut());
        }
    }
}

struct FeedWidget {
    app_event_tx: Sender<AppEvent>,

    show_help: bool,

    data: Arc<RwLock<FeedWidgetData>>,
    loading_count: Arc<AtomicUsize>,
    http_client: Client,

    tb_state: TableState,
    tb_cum_row_heights: Vec<usize>, // Cumulative rendered height of each row in the table
    sb_state: ScrollbarState,

    exp_item: ExpandedItemWidget,
}

#[derive(Default)]
struct FeedWidgetData {
    items: Vec<FeedItem>,
}

enum Feed {
    Atom(atom_syndication::Feed),
    Rss(rss::Channel),
}

impl FeedWidget {
    const HTTP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("RSSTERM_VERSION"),);

    fn new(app_event_tx: Sender<AppEvent>) -> Self {
        let http_client = Client::builder()
            .user_agent(Self::HTTP_USER_AGENT)
            .build()
            .expect("Failed to create HTTP client");
        Self {
            app_event_tx,
            http_client,
            show_help: false,
            data: Arc::new(RwLock::new(FeedWidgetData::default())),
            loading_count: Arc::new(AtomicUsize::new(0)),
            tb_state: TableState::default(),
            tb_cum_row_heights: Vec::new(),
            sb_state: ScrollbarState::default(),
            exp_item: ExpandedItemWidget::default(),
        }
    }

    fn run(&mut self, chan_urls: Vec<String>) {
        if chan_urls.is_empty() {
            self.show_help = true;
            return;
        }

        let http_client = self.http_client.clone();
        let data = Arc::clone(&self.data);

        let loading_count = Arc::clone(&self.loading_count);
        loading_count.store(chan_urls.len(), Ordering::SeqCst);

        tokio::spawn(async move {
            let mut query_set: JoinSet<Result<Feed, Box<dyn Error + Send + Sync>>> = JoinSet::new();

            for chan_url in chan_urls {
                let local_http_client = http_client.clone();
                query_set.spawn(async move {
                    let http_resp = local_http_client.get(chan_url).send().await?;
                    let http_resp_bytes = &http_resp.bytes().await?[..];
                    match rss::Channel::read_from(http_resp_bytes) {
                        Ok(rss_feed) => Ok(Feed::Rss(rss_feed)),
                        Err(_) => match atom_syndication::Feed::read_from(http_resp_bytes) {
                            Ok(atom_feed) => Ok(Feed::Atom(atom_feed)),
                            Err(_) => Err(Box::from("Failed to parse feed")),
                        },
                    }
                });
            }

            while let Some(result) = query_set.join_next().await {
                match result {
                    Ok(Ok(parsed_feed)) => {
                        let new_items: Vec<_> = match parsed_feed {
                            Feed::Atom(atom_feed) => atom_feed
                                .entries()
                                .iter()
                                .filter_map(FeedItem::from_atom_entry)
                                .collect(),
                            Feed::Rss(rss_feed) => rss_feed
                                .items()
                                .iter()
                                .filter_map(FeedItem::from_rss_item)
                                .collect(),
                        };
                        let mut data = data.write().unwrap();
                        data.items.extend(new_items);
                        data.items.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
                    }
                    Ok(Err(e)) => eprintln!("Feed fetch error: {}", e),
                    Err(e) => eprintln!("Task failed: {}", e),
                }
                loading_count.fetch_sub(1, Ordering::SeqCst);
            }
        });
    }

    fn is_loading(&self) -> bool {
        self.loading_count.load(Ordering::SeqCst) > 0
    }

    async fn handle_event(&mut self, event: AppEvent) {
        let is_exp_item_active = self.exp_item.id.is_some();
        match event {
            AppEvent::Scroll(delta) => {
                if is_exp_item_active {
                    self.exp_item.scroll(delta);
                } else {
                    self.scroll_feed(delta);
                }
            }
            AppEvent::Expand => {
                let items = &self.data.read().unwrap().items;
                if let Some(selected_item_i) = self.tb_state.selected() {
                    if let Some(feed_item) = items.get(selected_item_i) {
                        self.exp_item.id = Some(feed_item.id);
                    }
                }
            }
            AppEvent::Close => {
                if self.exp_item.id.is_some() {
                    self.exp_item = ExpandedItemWidget::default();
                } else {
                    // If the feed widget does not have a nested view that can be closed, we send a exit
                    // event upstream. We can do this because if a widget receives an event, it is the
                    // only active/focused widget of the entire app, as such the widget can safely
                    // determine whether to exit the app
                    self.app_event_tx.send(AppEvent::Exit).await.ok();
                }
            }
            AppEvent::Open => self.open_selected(),
            _ => (),
        }
    }

    fn scroll_feed(&mut self, delta: isize) {
        match delta {
            isize::MIN => self.tb_state.select_first(),
            isize::MAX => self.tb_state.select_last(),
            delta if delta < 0 => self.tb_state.scroll_up_by((-delta) as u16),
            delta => self.tb_state.scroll_down_by(delta as u16),
        }
        // NOTE: The range of selected_i is [0, data.len() - 1]
        // This is likely to allow developers to catch overflow events to handle wrap arounds
        // Currently, we are not allowing wrap arounds, hence we are clamping the value
        let selected_item_i = self
            .tb_state
            .selected()
            .unwrap_or(0)
            .clamp(0, self.tb_cum_row_heights.len().saturating_sub(1));
        // If the first item is selected, there should be no scrollbar movement (i.e. position 0)
        self.sb_state = self.sb_state.position(
            self.tb_cum_row_heights
                .get(selected_item_i.saturating_sub(1))
                .unwrap_or(&0)
                * min(selected_item_i, 1),
        );
    }

    fn open_selected(&self) {
        let items = &self.data.read().unwrap().items;

        let open_result = self
            .tb_state
            .selected()
            .and_then(|i| items.get(i))
            .and_then(|item| item.url.as_ref())
            .map(|url| open::that(url));

        match open_result {
            Some(Err(e)) => eprintln!("Failed to open URL: {}", e),
            None => eprintln!("No item selected or no URL available"),
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        if self.show_help {
            let help_para = para_wrap!(text![
                line!["NO FEEDS FOUND"].bold(),
                line!(),
                line!["Add RSS/Atom URLs to the feeds file to get started"].fg(WARM_WHITE_RGB),
                line!(),
                line![
                    span!("$ ").dim(),
                    span!("echo 'https://hnrss.org/frontpage' >> $(rssterm feeds)").green()
                ],
            ])
            .block(Block::default().padding(Padding {
                top: area.height / 3,
                ..Padding::ZERO
            }))
            .centered();

            return frame.render_widget(help_para, area);
        }

        let feed_items = &self.data.read().unwrap().items;

        if let Some(exp_feed_item) = self
            .exp_item
            .id
            .and_then(|id| feed_items.iter().find(|item| item.id == id))
        {
            return self.exp_item.render(frame, area, exp_feed_item);
        }

        let [tb_area, sb_area] = horizontal![*=1, ==2].areas(area);

        let tb_col_spacing = 2;
        let tb_col_layout = constraints![*=0, ==20%];

        let tb_hl_symbol = ">> ";
        let tb_hl_symbol_len = tb_hl_symbol.len() as u16;

        // Dynamically calculate the rendered width of each table column, required for text wrapping
        let tb_col_areas: [Rect; 2] = Layout::horizontal(tb_col_layout)
            .spacing(tb_col_spacing)
            .areas(Rect {
                x: tb_area.x + tb_hl_symbol_len,
                width: tb_area.width.saturating_sub(tb_hl_symbol_len),
                ..tb_area
            });

        self.tb_cum_row_heights.resize(feed_items.len(), 0);

        let mut tbl_total_content_height = 0;
        let tb_rows: Vec<Row> = feed_items
            .iter()
            .enumerate()
            .map(|(i, feed_item)| {
                let (tb_row, tb_row_h) = feed_item.draw_row(&tb_col_areas);

                let tb_row_btm_margin = (!(i == feed_items.len().saturating_sub(1))) as u16;
                let tb_row_total_h = tb_row_h + tb_row_btm_margin;
                tbl_total_content_height += tb_row_total_h as usize;

                // Each row has a dynamic height determined by text wrapping. Therefore, cumulative row
                // heights are updated every render cycle
                self.tb_cum_row_heights[i] = tbl_total_content_height;
                tb_row.bottom_margin(tb_row_btm_margin)
            })
            .collect();

        self.sb_state = self.sb_state.content_length(tbl_total_content_height);

        // Select the expanded item if available, otherwise select first item if none selected
        let selected_item_index = self
            .exp_item
            .id
            .and_then(|item_id| feed_items.iter().position(|item| item.id == item_id))
            .or_else(|| match self.tb_state.selected() {
                None if !feed_items.is_empty() => Some(0),
                current => current,
            });
        self.tb_state.select(selected_item_index);

        let table = Table::new(tb_rows, tb_col_layout)
            .highlight_symbol(span!(tb_hl_symbol).magenta())
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
                Some(title_text) => {
                    wrap_then_apply(&title_text, title_width, |l| line!(l).white().bold())
                }
                None => wrap_then_apply(&"untitled".to_string(), title_width, |l| {
                    line!(l).dim().bold()
                }),
            }
        };

        let content_lines = match self.url {
            Some(ref url) => chain(w_title, vec![line!(url).dim()]).collect(),
            None => w_title,
        };

        let w_pub_date = wrap_then_apply(
            &HumanTime::from(self.pub_date).to_string(),
            pub_date_width as usize,
            |l| line!(l).yellow().italic().right_aligned(),
        );

        let row_height = max(content_lines.len(), w_pub_date.len()) as u16;
        (
            row![content_lines, w_pub_date].height(row_height),
            row_height,
        )
    }
}

#[derive(Clone, Default)]
struct ExpandedItemWidget {
    id: Option<NonZeroU64>,
    cached_render_content: Option<Vec<Line<'static>>>,

    curr_content_render_width: Option<u16>,
    curr_content_render_height: Option<u16>,

    scroll_offset: usize,
    sb_state: ScrollbarState,
}

impl ExpandedItemWidget {
    fn get_max_scroll_offset(&self) -> usize {
        self.cached_render_content
            .as_ref()
            .map_or(0, |content| content.len())
            .saturating_sub(self.curr_content_render_height.unwrap_or(0) as usize)
    }

    fn scroll(&mut self, delta: isize) {
        match delta {
            isize::MIN => self.scroll_offset = 0,
            isize::MAX => self.scroll_offset = self.get_max_scroll_offset(),
            delta if delta < 0 => {
                self.scroll_offset = self.scroll_offset.saturating_sub(delta.unsigned_abs())
            }
            delta => {
                self.scroll_offset =
                    (self.scroll_offset + delta as usize).min(self.get_max_scroll_offset());
            }
        }
        self.sb_state = self.sb_state.position(self.scroll_offset);
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, feed_item: &FeedItem) {
        let outline_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Color::DarkGray)
            .padding(Padding::symmetric(2, 1));

        let render_area = outline_block.inner(area);
        // Dynamically wrap the title to calculate height required for full visibility.
        // `Paragraph::wrap` is not enough to guarantee visibility if the allocated area is smaller than
        // the wrapped text. Therefore, we will need to dynamically set the height of the render area for the title
        let title_lines = match &feed_item.title {
            Some(title_text) => wrap_then_apply(title_text, render_area.width as usize, |l| {
                line!(l).white().bold()
            }),
            None => vec![line!("untitled").dim().bold()],
        };

        let title_h = title_lines.len() as u16;
        // Assume that metadata will only ever take up 2 lines. This is not ideal as there will be a
        // breaking point where parts of metadata will be hidden if the width of the terminal is too small
        let meta_h: u16 = 2;

        let [header_area, _, content_area, _]: [Rect; 4] =
            // +1: padding between title and metadata
            vertical![==(title_h + meta_h + 1), ==1, *=0, ==1].areas(render_area);

        let [title_area, _, meta_area]: [Rect; 3] =
            vertical![==title_h, ==1, ==meta_h].areas(header_area);

        let [left_meta_area, right_meta_area]: [Rect; 2] = horizontal![==50%, ==50%]
            .flex(Flex::SpaceBetween)
            .areas(meta_area);

        frame.render_widget(outline_block, area);
        frame.render_widget(Text::from(title_lines), title_area);

        let pub_date_label = para_wrap!(text![
            line!(HumanTime::from(feed_item.pub_date).to_string())
                .yellow()
                .italic(),
            line!(feed_item.pub_date.format(LONG_TIMESTAMP_FMT).to_string()).dim()
        ]);

        if !feed_item.authors.is_empty() {
            let mut author_spans = vec![span!("by ").dim()];
            for (i, author) in feed_item.authors.iter().enumerate() {
                if i > 0 {
                    author_spans.push(span!(", ").dim());
                }
                author_spans.push(span!(author).light_green().italic());
            }
            frame.render_widget(para_wrap!(text!(author_spans)), left_meta_area);
            frame.render_widget(pub_date_label.right_aligned(), right_meta_area);
        } else {
            frame.render_widget(pub_date_label.left_aligned(), left_meta_area);
        }

        let [text_area, sb_area] = horizontal![*=1, ==2].areas(content_area);

        let content = self.sync_content_and_viewport(feed_item, text_area);
        let content_height = content.len();

        let visible_content = content
            .into_owned()
            .into_iter()
            .skip(self.scroll_offset)
            .take(text_area.height as usize)
            .collect::<Vec<_>>();

        frame.render_widget(Text::from(visible_content), text_area);

        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐")
            .thumb_style(Color::DarkGray);

        let scrollable_height = content_height.saturating_sub(text_area.height as usize);
        self.sb_state = self.sb_state.content_length(scrollable_height);

        frame.render_stateful_widget(scrollbar, sb_area, &mut self.sb_state);
    }

    fn sync_content_and_viewport(
        &mut self,
        feed_item: &FeedItem,
        render_area: Rect,
    ) -> Cow<[Line<'static>]> {
        let render_width_changed = match self.curr_content_render_width {
            Some(curr_render_width) => curr_render_width != render_area.width,
            None => true,
        };
        let item_id_changed = self.id != Some(feed_item.id);

        if render_width_changed || item_id_changed {
            let content_to_render = feed_item
                .content
                .as_deref()
                .or(feed_item.description.as_deref());

            self.cached_render_content = content_to_render.map(|content| {
                content
                    .iter()
                    .flat_map(|l| {
                        wrap_then_apply(l, render_area.width as usize, |l| {
                            line!(l).fg(WARM_WHITE_RGB)
                        })
                    })
                    .collect()
            });
        }

        self.id = Some(feed_item.id);
        self.curr_content_render_height = Some(render_area.height);
        self.curr_content_render_width = Some(render_area.width);

        // Ensure that the scroll offset is within the bounds of the content
        self.scroll_offset = self.scroll_offset.min(self.get_max_scroll_offset());
        self.sb_state = self.sb_state.position(self.scroll_offset);

        Cow::Borrowed(self.cached_render_content.as_ref().unwrap())
    }
}

#[derive(Clone)]
struct FeedItem {
    id: NonZeroU64,
    title: Option<String>,
    url: Option<String>,
    authors: Vec<String>,
    description: Option<Vec<String>>,
    content: Option<Vec<String>>,
    pub_date: DateTime<chrono::Local>,
}

impl FeedItem {
    fn from_atom_entry(entry: &atom_syndication::Entry) -> Option<Self> {
        let url = entry
            .links
            .iter()
            .find(|link| link.rel == "alternate")
            .or_else(|| entry.links.first())
            .map(|link| link.href.to_owned());

        let mut hasher = DefaultHasher::default();
        (&entry.id, &entry.title.value, &entry.updated).hash(&mut hasher);

        Some(Self {
            id: NonZero::new(hasher.finish()).unwrap(),
            title: Some(entry.title.value.to_owned()),
            authors: entry
                .authors
                .iter()
                .map(|author| author.name.to_owned())
                .collect(),
            description: entry.summary().map(|desc| try_parse_html(&desc.value)),
            content: entry
                .content()
                .and_then(|c| c.value())
                .map(|c_str| try_parse_html(c_str)),
            url,
            pub_date: entry.updated.into(),
        })
    }

    fn from_rss_item(item: &rss::Item) -> Option<Self> {
        let mut authors = match item.dublin_core_ext {
            Some(ref dcmi_ext) => dcmi_ext
                .creators()
                .iter()
                .map(|creator| str::to_string(creator))
                .collect(),
            None => Vec::new(),
        };
        // Prioritise dublin core metadata (dcmi) over RSS metadata
        // This is just a guess, but it seems like the dcmi is more reliable and more widely used based
        // on the feeds I am subscribed to
        if authors.is_empty() {
            item.author().map(|author| authors.push(author.to_string()));
        }

        let mut hasher = DefaultHasher::default();
        (&item.title, &item.description, &item.pub_date).hash(&mut hasher);

        Some(Self {
            id: NonZero::new(hasher.finish()).unwrap(),
            title: item.title().map(str::to_string),
            url: item.link().map(str::to_string),
            pub_date: DateTime::parse_from_rfc2822(item.pub_date()?).ok()?.into(),
            description: item.description().map(try_parse_html),
            content: item.content().map(try_parse_html),
            authors,
        })
    }
}
