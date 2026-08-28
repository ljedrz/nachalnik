//! The little the networked examples share that is theirs alone: two formatting helpers.
//!
//! note: The provider they talk through used to live here, five hundred lines of it, and a second
//! copy of the same thing lived in `tests/live.rs`. Both now come from `nachalnik-utils`, a
//! workspace member that is never published and exists only so that this crate's own scaffolding
//! is written once. What is left here is presentation, which is the examples' own business.
//!
//! It is pulled in with `#[path = "common/mod.rs"] mod common;`, because a directory under
//! `examples/` with no `main.rs` is not built as an example of its own.

// each example uses a different part of this
#![allow(dead_code, unused_imports)]

pub use nachalnik_utils::{base_url, models, providers};

/// Formats a number with `,` as the thousands separator.
pub fn thousands(n: impl TryInto<u64>) -> String {
    let digits = n.try_into().unwrap_or(0).to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }

    out
}

/// Wraps text to a width, indenting every line.
pub fn wrap(text: &str, width: usize, indent: &str) -> String {
    let mut out = String::new();

    for paragraph in text.split('\n') {
        let mut column = 0;
        for word in paragraph.split_whitespace() {
            if column == 0 {
                out.push_str(indent);
            } else if column + 1 + word.chars().count() > width {
                out.push('\n');
                out.push_str(indent);
                column = 0;
            } else {
                out.push(' ');
                column += 1;
            }
            out.push_str(word);
            column += word.chars().count();
        }
        out.push('\n');
    }

    out.trim_end().to_owned()
}
