use textwrap::{Options, wrap};

pub(crate) fn wrap_then_apply<T>(text: &String, width: usize, apply: fn(String) -> T) -> Vec<T> {
    wrap(text, Options::new(width).break_words(true))
        .into_iter()
        .map(|cow| apply(cow.into_owned()))
        .collect()
}
