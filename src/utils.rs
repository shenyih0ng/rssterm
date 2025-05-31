use textwrap::{Options, wrap};

pub(crate) fn wrap_then_apply<T>(text: &str, width: usize, apply: fn(String) -> T) -> Vec<T> {
    wrap(text, Options::new(width).break_words(true))
        .into_iter()
        .map(|line_str| apply(line_str.to_string()))
        .collect()
}

pub(crate) fn wrap_then_apply_vec<T>(
    lines: &[String],
    width: usize,
    apply: fn(String) -> T,
) -> Vec<T> {
    lines
        .iter()
        .flat_map(|line| wrap_then_apply(line, width, apply))
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
