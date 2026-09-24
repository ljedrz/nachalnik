//! The little the networked examples share that is theirs alone: two formatting helpers, and
//! what `--save` writes.
//!
//! note: The provider they talk through is `nachalnik-providers`, a published crate, and the
//! environment it is built from comes from `nachalnik-utils`, which is never published and exists
//! so that this crate's own scaffolding is written once; the part of it the examples call is
//! re-exported below. What is left here is presentation and the files `--save` writes, which are
//! the examples' own business.
//!
//! It is pulled in with `#[path = "common/mod.rs"] mod common;`, because a directory under
//! `examples/` with no `main.rs` is not built as an example of its own.

// each example uses a different part of this
#![allow(dead_code, unused_imports)]

use nachalnik::{BoxError, Kernel};
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

/// Writes every session where it can be read back, each under its model's name: the log as it
/// happened, and the snapshot as it ended up.
pub fn save<'a>(
    sessions: impl IntoIterator<Item = (&'a str, &'a Kernel)>,
    dir: &str,
) -> Result<(), BoxError> {
    std::fs::create_dir_all(dir)?;

    for (model, kernel) in sessions {
        let slug: String = model
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();

        let log: Vec<String> = kernel
            .history()
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        std::fs::write(format!("{dir}/{slug}.jsonl"), log.join("\n"))?;
        std::fs::write(
            format!("{dir}/{slug}.json"),
            serde_json::to_vec_pretty(&kernel.snapshot())?,
        )?;

        println!("  wrote {dir}/{slug}.jsonl and {dir}/{slug}.json");
    }

    Ok(())
}
