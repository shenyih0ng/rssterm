use textwrap::{Options, wrap};

pub(crate) fn wrap_then_apply<T>(text: &String, width: usize, apply: fn(String) -> T) -> Vec<T> {
    wrap(text, Options::new(width).break_words(true))
        .into_iter()
        .map(|cow| apply(cow.into_owned()))
        .collect()
}

pub(crate) fn parse_html_or(html: &str, default: String) -> String {
    html2text::config::plain()
        .no_link_wrapping()
        .link_footnotes(true)
        .string_from_read(html.as_bytes(), usize::MAX)
        .unwrap_or(default)
}

#[macro_export]
macro_rules! para_wrap {
    () => {{ ::ratatui::widgets::Paragraph::default().wrap(Wrap { trim: true }) }};
    ($text:expr) => {{ ::ratatui::widgets::Paragraph::new($text).wrap(Wrap { trim: true }) }};
}
