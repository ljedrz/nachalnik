//! Handing a piece of the session to the terminal's own clipboard.
//!
//! note: a screen selection is a rectangle: the frame on both sides, text wrapped to the window,
//! and nothing scrolled out. Pasting it means deleting `│` from the front and back of every line.
//! This program holds the answer unwrapped and whole, so it can hand that over instead.
//!
//! note: OSC 52 is an escape sequence, not a library. The text goes to the terminal base64-encoded,
//! and the terminal puts it where a paste will find it. It costs no dependency and works over
//! `ssh`; a clipboard crate talking to this machine's display server does not, because the terminal
//! at the other end holds the clipboard.
//!
//! note: it cannot find out whether it worked. There is no reply: a terminal that does not
//! implement the sequence, or has it switched off, drops it and says nothing. `foot` and
//! `alacritty` take it; `tmux` forwards it only with `set-clipboard on`; Apple's Terminal has never
//! had it. The program reports that it handed the text to the terminal, never that the clipboard
//! holds it.

use std::io::{IsTerminal, Write};

use base64::{Engine, engine::general_purpose::STANDARD};

/// What a terminal reads as "put this on the clipboard".
///
/// note: `c` is the selection a paste reads from. The others are the primary selection and cut
/// buffers; writing to them would change something the user did not ask about.
///
/// note: terminated with `BEL`, not `ESC \`. Both are in the specification, but every
/// implementation accepts `BEL`.
pub fn sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}

/// Writes it to the terminal, or says why there was none to write to.
///
/// note: uses stderr, the terminal stream in both loops. The headless loop writes the session log
/// to stdout, one JSON record per line, so an escape sequence there would be a line nothing can
/// parse. stderr works whichever loop is running.
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
        // the payload is the base64 encoding, not the text: a newline inside an escape sequence
        // would end it
        assert_eq!(sequence("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(sequence("two\nlines"), "\x1b]52;c;dHdvCmxpbmVz\x07");
        assert!(!sequence("two\nlines").contains('\n'));
    }
}
