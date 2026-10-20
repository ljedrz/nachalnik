//! The little the examples share that is theirs alone: how they draw, and what `--save` writes.
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

/// How wide the examples draw.
pub const WIDTH: usize = 78;

/// A numbered stage, with a sentence saying what it is for.
pub fn stage(number: usize, title: &str, note: &str) {
    println!("\n\x1b[1m{number} · {title}\x1b[0m");
    for line in textwrap(note, WIDTH - 4) {
        println!("   \x1b[2m{line}\x1b[0m");
    }
    println!();
}

pub fn textwrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().expect("there is always one");
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(word.to_owned());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }

    lines
}

/// A remark under a table, in the same voice as the stage headings.
pub fn note(text: &str) {
    for (i, line) in textwrap(text, WIDTH - 6).into_iter().enumerate() {
        let lead = if i == 0 { "→" } else { " " };
        println!("   \x1b[2m{lead} {line}\x1b[0m");
    }
}

pub fn heading(text: &str) {
    println!("\n\x1b[1m{text}\x1b[0m");
}

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
