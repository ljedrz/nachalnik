//! The words out of a body that is not JSON.
//!
//! note: a module of its own rather than a function in `reading`, because the System One client
//! also reads refusals and is built without either chat dialect.

/// The words out of a body that is not JSON, with any markup around them taken off.
///
/// note: for the address that is a web page rather than an API, which is what a mistyped
/// `base_url` produces. `https://example.com/v1` answers 405 with a whole HTML document, and the
/// first three hundred characters of it are a doctype, a `<link rel=icon>` and the opening of a
/// stylesheet - noise in the conversation, the session log and the file somebody sends on.
///
/// note: the *words* rather than a refusal to show any, because a short page is often the only
/// account there is - `<html>gateway timeout</html>` from a proxy that speaks no JSON says the
/// one thing worth knowing. What goes is the tags, and the contents of `<style>` and `<script>`,
/// which are markup wearing the shape of text.
///
/// note: not an HTML parser and not trying to be. A body that is not markup passes through with
/// its whitespace collapsed, which is what a plain-text error wants anyway.
///
/// note: linear, and over the first [`READ`] bytes. Every caller keeps a few hundred characters of
/// what this returns, and a page it is handed can be megabytes of markup - a lowercased copy of the
/// rest at every `<` was gigabytes of copying for one error message.
pub(crate) fn unmarked(body: &str) -> String {
    let end = (0..=READ.min(body.len()))
        .rev()
        .find(|at| body.is_char_boundary(*at))
        .unwrap_or(0);
    let mut out = String::new();
    let mut rest = body[..end].trim();

    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        out.push(' ');
        rest = &rest[at..];

        // a `<style>` or `<script>` is skipped whole: its contents are not prose, and taking
        // only the tags off would leave the stylesheet behind as if it were
        let named = rest[1..].trim_start();
        let skip = ["style", "script"]
            .into_iter()
            .find(|element| names(named.as_bytes(), element.as_bytes()));
        rest = match skip {
            Some(element) => match closing(rest, element) {
                Some(end) => &rest[end..],
                // an unclosed one runs to the end, and the end is where this stops
                None => "",
            },
            None => rest,
        };
        // and past the tag itself, or - for a `<` that never closes - past the `<`, so that
        // `retry in <60s` keeps what follows it. In a body cut at `READ` it is the last tag, cut
        // short, and its name and attributes are not words
        rest = match rest.find('>') {
            Some(end) => &rest[end + 1..],
            None if end < body.len() => "",
            None => rest.get(1..).unwrap_or_default(),
        };
    }
    out.push_str(rest);

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The start of `text`, as much of it as an error quotes.
///
/// note: one bound for every error that quotes a body, so refusals are the same length whichever
/// path finds them.
pub(crate) fn quoted(text: &str) -> String {
    text.chars().take(QUOTED).collect()
}

/// How many characters of a body an error quotes.
const QUOTED: usize = 300;

/// A refused request's status, with the first words of its body after it if there are any.
pub(crate) fn status_and_words(status: impl std::fmt::Display, body: &str) -> String {
    let words = quoted(&unmarked(body));
    match words.is_empty() {
        true => status.to_string(),
        false => format!("{status}: {words}"),
    }
}

/// How much of a body [`unmarked`] reads.
const READ: usize = 64 << 10;

/// Where `</element` starts in `text`, in any case.
fn closing(text: &str, element: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len())
        .find(|&at| bytes[at..].starts_with(b"</") && names(&bytes[at + 2..], element.as_bytes()))
}

/// Whether the name at the start of `tag` is `element`, in any case.
///
/// note: the whole name, so `<stylesheet-error>` is treated as any other tag rather than as a
/// stylesheet whose contents are skipped.
fn names(tag: &[u8], element: &[u8]) -> bool {
    tag.get(..element.len())
        .is_some_and(|it| it.eq_ignore_ascii_case(element))
        && tag
            .get(element.len())
            .is_none_or(|next| *next == b'>' || *next == b'/' || next.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tag the cut at [`READ`] lands inside is not read as words, and a `<` that never closes in
    /// a body read whole still is.
    #[test]
    fn a_tag_cut_short_is_not_words() {
        let body = format!(
            "<p>quota exceeded</p><svg><path d=\"{}\"/></svg>",
            "M0 0".repeat(READ)
        );
        assert_eq!(unmarked(&body), "quota exceeded");

        assert_eq!(unmarked("busy, retry in <60s"), "busy, retry in 60s");
    }
}
