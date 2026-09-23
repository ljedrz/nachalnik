//! Handing a piece of the session to the terminal's own clipboard.
//!
//! note: the problem this is for is that a screen is a rectangle and a selection over one is a
//! rectangle too. What a mouse drags across the chat tab is the frame down both sides of it, the
//! text wrapped to whatever the window happened to be, and nothing that has scrolled out of the
//! pane - so pasting a model's answer somewhere means deleting a `│` from the front and the back
//! of every line of it. This program is holding the answer itself, unwrapped and whole, and can
//! hand that over instead.
//!
//! note: OSC 52, which is an escape sequence rather than a library: the text goes to the terminal
//! base64-encoded and the terminal puts it where a paste will find it. It costs no dependency,
//! and it works over `ssh`, which a clipboard crate talking to this machine's display server does
//! not - the terminal at the other end is the one with the clipboard somebody will paste into.
//!
//! note: **it cannot find out whether it worked.** There is no reply to read: a terminal that
//! does not implement the sequence, or that has it switched off, drops it and says nothing. `foot`
//! and `alacritty` take it, `tmux` forwards it only with `set-clipboard on`, and Apple's Terminal
//! has never had it. So what this program says afterwards is what it *did* - that it handed the
//! text to the terminal - and never that the clipboard now holds it, which is a thing it does not
//! know.

use std::io::{IsTerminal, Write};

use base64::{Engine, engine::general_purpose::STANDARD};

/// What a terminal reads as "put this on the clipboard".
///
/// note: `c`, which is the selection a paste reads from. The other letters are the primary
/// selection and the cut buffers, and a program that wrote to those would be answering a key
/// somebody pressed by changing something they did not ask about.
///
/// note: terminated with `BEL` rather than `ESC \`. Both are in the specification and the second
/// is the tidier one; the first is what every implementation of this accepts.
pub fn sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}

/// Writes it to the terminal, or says why there was none to write to.
///
/// note: stderr, which is the stream that is the terminal in both of this program's loops. The
/// headless one writes the session log to stdout, one JSON record a line, and an escape sequence
/// in the middle of that is a line nothing can parse - so the one place this can go without
/// depending on which loop is running is the other one.
pub fn hand_over(text: &str) -> Result<(), String> {
    let mut out = std::io::stderr();
    if !out.is_terminal() {
        return Err("there is no terminal here for it to go to".to_owned());
    }

    out.write_all(sequence(text).as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_the_sequence_a_terminal_reads() {
        // the shape, and that the payload is the encoding rather than the text: a newline inside
        // an escape sequence would end it, which is why this is base64 at all
        assert_eq!(sequence("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(sequence("two\nlines"), "\x1b]52;c;dHdvCmxpbmVz\x07");
        assert!(!sequence("two\nlines").contains('\n'));
    }
}
