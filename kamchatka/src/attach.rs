//! Putting a file in the context: text as text, and everything else as bytes.
//!
//! note: one function, reached two ways. `-f` at startup and `/attach` at the prompt are the same
//! act at two moments. The alternative - a flag that reads text and a command that reads bytes -
//! would mean `-f report.pdf` failing with a decoding error at the one moment a person has the
//! least idea what this program can do. So `-f` grew the whole of it instead: whatever the file
//! is, it goes in as what it is.
//!
//! note: what decides is the extension, and only for the types listed below. Sniffing the content
//! was the obvious alternative and it gets the interesting case wrong: an uncompressed PDF is
//! valid UTF-8 for pages at a time, so "is it text?" answers yes and sends the model PDF source
//! where a provider has a part that would have carried the document. A short table that says what
//! this program is prepared to name is the honest shape - a media type is a claim, and guessing
//! one is building a request the model cannot read.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nachalnik::{Blob, Block, Content, ContextItem};
use serde_json::json;

/// The most a single attachment may be, on disk.
///
/// note: base64 is a third larger again, so this is a little over 21 MB on the wire, which is
/// past what the endpoints accept anyway. What the cap is really for is the other failure: with
/// none, a mistyped path at a video file builds a request nothing will take, spends a minute
/// uploading it, and leaves a session log that cannot be written. Refusing costs a sentence.
const MOST: u64 = 16 * 1024 * 1024;

/// What this program is prepared to call a file, by extension.
///
/// note: short on purpose. Every entry is a claim that this media type is what an endpoint should
/// be told, and the ones here are the ones the dialects next door actually carry: pictures and
/// documents everywhere, recordings where Google's own API is being spoken. Anything not named
/// here is offered to the model as text, which is the right answer for the overwhelming majority
/// of what a person points this at - source, markdown, logs, CSV, JSON.
const TYPES: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("mp4", "video/mp4"),
];

/// The media type for a path, if this program is prepared to name one.
fn media_type(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?.to_lowercase();

    TYPES
        .iter()
        .find(|(suffix, _)| *suffix == extension)
        .map(|(_, media_type)| *media_type)
}

/// Reads a file and builds the context item for it - a text reference, or one carrying bytes.
///
/// The item is neither pinned nor given a reason; that is the caller's, because the two callers
/// have different ones to give.
///
/// note: the bytes case is a [`Content::Blocks`] of two - the path, then the payload - rather
/// than a bare [`Content::Blob`], and the extra block is not decoration.
/// [`LinearProjector`](nachalnik::LinearProjector) labels a reference by prepending its label to
/// the *text*, so a reference that is not text loses its label on the way out: the model would be
/// handed a document with nothing saying which file it was, in a conversation where the person had
/// just typed the name. Both dialects already carry a sentence beside a payload - it is the shape
/// a turn takes when it is a question about a screenshot - so the name travels as one.
pub fn attached(path: &str) -> Result<ContextItem> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("could not read {path}"))?
        .len();
    if size > MOST {
        bail!(
            "{path} is {} MB, and this will not attach anything over {} MB: what goes on the wire \
             is a third larger again, and no endpoint here would take it",
            size / (1024 * 1024),
            MOST / (1024 * 1024),
        );
    }

    let Some(media_type) = media_type(path) else {
        // the same read `-f` has always done, and the same error when the file is not text: what
        // has changed is that there is now a list of things it is not an error for
        let content =
            std::fs::read_to_string(path).with_context(|| format!("could not read {path}"))?;

        return Ok(ContextItem::file(path, content));
    };

    let bytes = std::fs::read(path).with_context(|| format!("could not read {path}"))?;
    // note: `name` rather than the whole path, because the one thing reading it is the attachment
    // part of the conventional dialect, whose `filename` is a label on a file and not a location
    // on this machine. See `Blob::meta`: nothing in the runtime knows this key
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    let blob = Blob::new(media_type, STANDARD.encode(&bytes)).with_meta(json!({ "name": name }));

    Ok(ContextItem::file(
        path,
        Content::blocks([
            Block::text(Content::text(format!("{path}:"))),
            Block::text(Content::Blob(blob.into())),
        ]),
    ))
}

/// What went in, for the line that says so: the media type and the size on disk, or `None` for a
/// file that went in as text.
///
/// note: read back off the item rather than returned beside it, so that the sentence the person
/// reads cannot describe something other than what was pushed.
pub fn describe(item: &ContextItem) -> Option<String> {
    let blobs = item.content.blobs();
    let blob = blobs.first()?;

    // the payload is base64 and the person is thinking of the file, so this is the length they
    // would see in a directory listing rather than the one that goes on the wire
    let bytes = blob.byte_len() * 3 / 4;

    Some(match bytes >= 1024 {
        true => format!("{}, {} KB", blob.media_type, bytes / 1024),
        // note: and not `0 KB`, which is what a small one rounded to. An eight-pixel PNG is a
        // hundred bytes and can still be 255 tokens at a vendor charging by the tile, so a size
        // that reads as nothing at all is the wrong impression to leave beside a figure that is
        // already saying nobody could price it
        false => format!("{}, {bytes} bytes", blob.media_type),
    })
}
