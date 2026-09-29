//! How a message is framed: one JSON value to a line, no line longer than [`MAX_LINE`], and a
//! reader that stops at the cap rather than after it.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// The longest line either end will read before giving up on the connection.
///
/// note: generous, and it has to be. A frame here is not something somebody typed - it is whatever
/// the session put in its log, and `context.replaced` is the one event that carries content, so a
/// rewritten tool result goes down the wire at whatever size it was. What this is defending
/// against is a peer that never sends a newline, which would otherwise be read into memory for
/// ever; a frame over it is a protocol error that closes the connection and says so, rather than a
/// truncation that would leave the reader parsing the second half of somebody's JSON.
///
/// note: enforced while the frame arrives rather than once it has, which is what [`Frames`] is for
/// and why it is not `tokio::io::Lines`. Checked afterwards it is no defence against the case
/// above at all, because the reading is the thing it was supposed to stop.
///
/// note: the reading side enforces this and the writing side *names* it. Nothing caps what the
/// session writes into its log and nothing could: a record is in the log, the log drops nothing,
/// and a client resuming by sequence comes back to the same record every time - so a
/// `context.replaced` over this, sent as it is, makes a session unattachable for the rest of its
/// life. What goes out instead is [`Message::Oversized`](super::Message::Oversized), which names
/// the record and its size and lets the client move past it;
/// [`Command::Inspect`](super::Command::Inspect) is how the content is fetched when somebody wants
/// it. Raising the number is not the fix: it moves the size of the thing that breaks and changes
/// nothing else.
pub const MAX_LINE: usize = 32 * 1024 * 1024;

/// The longest path a `unix:PATH` address may name, in bytes.
///
/// note: a field of the kernel's own rather than anything chosen here. A unix address is a
/// `sockaddr_un`, and its `sun_path` is 108 bytes on Linux including the NUL that ends it - so a
/// path of 108 or more cannot be bound, and the bind says so in the kernel's words ("path must be
/// shorter than SUN_LEN") or not at all ("File name too long"), neither of which names the limit or
/// a shorter place to put the socket. `RUNNING.md` opens this whole section with a path under
/// `/run/user`, which is where a path of this size comes from: a deep runtime directory, a
/// container's id, a project directory somebody walked up to.
///
/// note: stated as the largest path rather than the size of the field, so the refusal can be one
/// sentence naming what was typed and what it has to be under.
pub const MAX_PATH: usize = 107;

/// Whether a `unix:` address is longer than the kernel can bind, and how long it is.
///
/// note: the same treatment [`MAX_LINE`] gets, and for the same reason: a limit nobody explains is
/// a limit a person cannot act on. Checked by both ends rather than left to the bind, because
/// `Server::unix` and the client's `connect` fail on the same path for the same reason
/// and one of them is a client that cannot be told what a session refuses.
pub fn overlong_path(path: &str) -> Option<usize> {
    (path.len() > MAX_PATH).then_some(path.len())
}

/// Whether a framed message is longer than the other end will read, and how long it is.
///
/// note: the newline is not part of what the reader measures. [`Frames`] holds the frame and
/// checks what it holds, which is everything up to the newline and not the newline, so the byte
/// that ends the line comes off before the comparison. One byte, and it decides whether a
/// record sitting exactly on the limit goes out or is named instead.
pub fn overlong(line: &[u8]) -> Option<usize> {
    let bytes = line.len().saturating_sub(1);

    (bytes > MAX_LINE).then_some(bytes)
}

/// A connection, read one frame at a time and no further than [`MAX_LINE`] into any of them.
///
/// note: not `tokio::io::Lines`, because of the cap. `next_line` grows its own buffer until a
/// newline arrives, so a limit over it can only ever be checked against a frame that has already
/// been read - which is no defence against the one case it exists for, a peer that never sends a
/// newline at all. This holds the part-read frame itself and stops as soon as
/// there is too much of it.
///
/// note: cancel-safe, which is what both loops that read commands need from it: the part-read
/// frame lives here rather than in the future, and `fill_buf` guarantees that a read dropped
/// before it resolved consumed nothing. So a `select!` that drops this mid-frame has lost nothing,
/// which is the property `Lines` had and the reason this can stand in for it.
pub struct Frames<R> {
    read: R,
    held: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> Frames<R> {
    /// One, over whatever this end of the connection reads as.
    pub fn new(read: R) -> Self {
        Self {
            read,
            held: Vec::new(),
        }
    }

    /// The next frame, or `None` where the peer has gone between two of them.
    async fn next(&mut self) -> Result<Option<String>, String> {
        loop {
            let available = self
                .read
                .fill_buf()
                .await
                .map_err(|e| format!("the connection stopped talking: {e}"))?;
            if available.is_empty() {
                return match self.held.is_empty() {
                    true => Ok(None),
                    // a peer that went away mid-frame, which is not the same thing as one that
                    // finished: half a message parses as nothing and says so
                    false => Err("the connection stopped in the middle of a message".to_owned()),
                };
            }
            let (whole, used) = match available.iter().position(|byte| *byte == b'\n') {
                Some(at) => {
                    self.held.extend_from_slice(&available[..at]);
                    (true, at + 1)
                }
                None => {
                    self.held.extend_from_slice(available);
                    (false, available.len())
                }
            };
            self.read.consume(used);
            // `at least`, because what is held is whatever had been buffered when the limit was
            // passed rather than the whole of what the peer meant to send - and the whole of it is
            // the number nobody here is ever going to know
            if self.held.len() > MAX_LINE {
                return Err(format!(
                    "a message of at least {} bytes, over the {MAX_LINE}-byte limit",
                    self.held.len()
                ));
            }
            if whole {
                // the `\r` of a `\r\n`, dropped where `Lines` drops it. Nothing here would notice
                // it - JSON reads it as whitespace either way - but this stands in for that reader,
                // and a stand-in that hands back a different string is a difference somebody finds
                // rather than one they read
                if self.held.last() == Some(&b'\r') {
                    self.held.pop();
                }

                return String::from_utf8(std::mem::take(&mut self.held))
                    .map(Some)
                    .map_err(|_| "a message that is not text".to_owned());
            }
        }
    }
}

/// Reads one message off a connection, or `None` where the peer has gone.
///
/// note: newline-delimited JSON, and the argument for it over a length prefix is that
/// `serde_json` escapes every control character it writes - a compact value never contains a
/// literal newline, so there is nothing for a delimiter to be confused by. What a length prefix
/// would buy is a payload that is not JSON, which this one is; what it costs is that the stream
/// stops being readable with the tools everybody already has. `nc | jq` is worth more than a
/// frame header here.
pub async fn read<T: for<'a> Deserialize<'a>>(
    frames: &mut Frames<impl AsyncBufRead + Unpin>,
) -> Result<Option<T>, String> {
    let Some(line) = frames.next().await? else {
        return Ok(None);
    };
    if line.trim().is_empty() {
        return Err("an empty message".to_owned());
    }

    serde_json::from_str(&line)
        .map(Some)
        .map_err(|e| format!("a message that is not one: {e}"))
}

/// Writes one message to a connection.
pub async fn write<T: Serialize>(
    out: &mut (impl AsyncWrite + Unpin),
    message: &T,
) -> Result<(), String> {
    write_frame(out, &framed(message)?).await
}

/// One message as the bytes it goes out as, newline and all.
///
/// note: split out of [`write()`] so that a caller can ask how long a message is before committing
/// to sending it, which is what `flush` does with a record; see [`overlong`]. Serializing twice to
/// answer that would be doing the expensive half twice on every record of every connection, and
/// the records this is about are the large ones.
pub fn framed<T: Serialize>(message: &T) -> Result<Vec<u8>, String> {
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');

    Ok(line)
}

/// Sends one already-framed message.
pub async fn write_frame(out: &mut (impl AsyncWrite + Unpin), line: &[u8]) -> Result<(), String> {
    out.write_all(line)
        .await
        .map_err(|e| format!("could not write to the connection: {e}"))?;
    out.flush()
        .await
        .map_err(|e| format!("could not write to the connection: {e}"))
}
