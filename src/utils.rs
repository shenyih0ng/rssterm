use std::time::{Duration, Instant};

use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::StatefulWidget};
use textwrap::{Options, wrap};
use throbber_widgets_tui::{Throbber as TuiThrobber, ThrobberState as TuiThrobberState};

pub const LONG_TIMESTAMP_FMT: &str = "%H:%M:%S / %-e-%b-%Y [%a]";
pub const WARM_WHITE_RGB: Color = Color::Rgb(232, 233, 240);

pub(crate) fn wrap_then_apply<T>(text: &str, width: usize, apply: fn(String) -> T) -> Vec<T> {
    wrap(text, Options::new(width).break_words(true))
        .into_iter()
        .map(|line_str| apply(line_str.to_string()))
        .collect()
}

pub(crate) fn try_parse_html(html: &str) -> Vec<String> {
    html2text::config::plain()
        .no_link_wrapping()
        .link_footnotes(true)
        // `html2text` does provide a `lines_from_read` method, however there isn't a good way to convert
        // lines to to `Vec<String>` directly.
        .string_from_read(html.as_bytes(), usize::MAX)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or(vec![html.to_owned()])
}

#[macro_export]
macro_rules! para_wrap {
    () => {{ ::ratatui::widgets::Paragraph::default().wrap(::ratatui::widgets::Wrap { trim: true }) }};
    ($text:expr) => {{ ::ratatui::widgets::Paragraph::new($text).wrap(::ratatui::widgets::Wrap { trim: true }) }};
}

pub(crate) struct Throbber {
    interval: Duration,

    _inner: TuiThrobberState,
    _last_instant: Instant,
}

impl Throbber {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            _inner: TuiThrobberState::default(),
            _last_instant: Instant::now(),
        }
    }

    pub fn render(&mut self, tui_throbber: TuiThrobber, area: Rect, buf: &mut Buffer) {
        if self._last_instant.elapsed() >= self.interval {
            self._inner.calc_next();
            self._last_instant = Instant::now();
        }
        tui_throbber.render(area, buf, &mut self._inner);
    }
}
