//! The words out of a body that is not JSON.
//!
//! note: a module of its own rather than a function in `reading`, because the System One client
//! reads a refusal too and is built without either chat dialect.

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
pub(crate) fn unmarked(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body.trim();

    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        out.push(' ');
        rest = &rest[at..];

        // a `<style>` or `<script>` is skipped whole: its contents are not prose, and taking
        // only the tags off would leave the stylesheet behind as if it were
        let skip = ["style", "script"].into_iter().find(|element| {
            rest[1..]
                .trim_start()
                .to_ascii_lowercase()
                .starts_with(*element)
        });
        rest = match skip {
            Some(element) => match rest.to_ascii_lowercase().find(&format!("</{element}")) {
                Some(end) => &rest[end..],
                // an unclosed one runs to the end, and the end is where this stops
                None => "",
            },
            None => rest,
        };
        // and past the tag itself, or - for a `<` that never closes - past the `<`
        rest = match rest.find('>') {
            Some(end) => &rest[end + 1..],
            None => "",
        };
    }
    out.push_str(rest);

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
