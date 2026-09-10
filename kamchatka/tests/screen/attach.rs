//! `/attach`: what reaches the context, and what reaches the request.
//!
//! note: the point of these is the split. A file this program has a media type for goes in as
//! bytes and a file it does not goes in as text, and the two are worth different things to
//! everything downstream - one is countable, readable in the pane and compactable, the other is
//! none of those and has to say so. A test that only checked an item arrived would pass for the
//! version that base64'd a markdown file.

use nachalnik::ModelResponse;

use crate::{common::scratch, harness::Harness};

/// A PNG's first eight bytes, which are not valid UTF-8 and are not meant to be.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Markdown goes in as text: counted, and readable where anybody looks for it.
#[tokio::test]
async fn a_markdown_file_is_attached_as_text() {
    let dir = scratch("attach-text");
    let path = dir.join("notes.md");
    std::fs::write(&path, "# what happened\n\nthe cache was cold.\n").expect("written");

    let mut harness = Harness::new([ModelResponse::text("read it")]);
    harness.send(&format!("/attach {}", path.display())).await;

    // the chat says what went in by reading the item, so there is one account of it and not two
    let screen = harness.flat();
    assert!(
        screen.contains("notes.md (file), 10 tokens"),
        "the derived line should name the file and what it costs: {screen}"
    );
    assert!(
        !screen.contains("nothing here can price"),
        "text is priced like anything else: {screen}"
    );

    let items = harness.app.kernel.items();
    let item = items.last().expect("the attachment");
    assert_eq!(
        item.content.as_text(),
        Some("# what happened\n\nthe cache was cold.\n"),
        "markdown is text and belongs in the context as text"
    );
    // and so it is priced like anything else, and says nothing is missing
    assert!(item.tokens > 0, "text is countable");
    assert_eq!(item.uncounted, 0);
}

/// A PDF goes in as bytes, and everything that reads a number afterwards says it could not.
///
/// note: the media type comes from the extension rather than from the content, which is the
/// decision this pins. An uncompressed PDF is valid UTF-8 for pages at a time, so a program that
/// asked "is this text?" would send the model PDF source - and the endpoint has a part that would
/// have carried the document.
#[tokio::test]
async fn a_pdf_is_attached_as_bytes_and_nothing_pretends_to_price_it() {
    let dir = scratch("attach-bytes");
    let path = dir.join("results.pdf");
    std::fs::write(
        &path,
        b"%PDF-1.4\nnot really a pdf, but it is not text either\n",
    )
    .expect("written");

    let mut harness = Harness::new([ModelResponse::text("read it")]);
    harness.send(&format!("/attach {}", path.display())).await;

    let screen = harness.flat();
    assert!(
        screen.contains("results.pdf (file), application/pdf"),
        "the line should name what went in: {screen}"
    );
    // the tokens on it are the path travelling beside the payload; the payload itself is the
    // piece nobody could put a number on, and the line says which is which
    assert!(
        screen.contains("tokens and 1 piece(s) nothing here can price"),
        "a `0` nobody explained is the one number here that reads as good news: {screen}"
    );

    let items = harness.app.kernel.items();
    let item = items.last().expect("the attachment");
    let blobs = item.content.blobs();
    let blob = blobs.first().expect("a blob");
    assert_eq!(&*blob.media_type, "application/pdf");
    // the name the producer knew, which is the one thing that can reach a `file` part's filename
    assert_eq!(
        blob.meta.get("name").and_then(|n| n.as_str()),
        Some("results.pdf")
    );
    assert_eq!(item.uncounted, 1, "one piece of it has no number on it");

    // and the whole budget says so, which is what `Trim` reads to decide it should run at all
    assert!(!harness.app.kernel.budget().fully_counted());
}

/// The path travels with the payload, because the projector cannot label a reference that is not
/// text and the model would otherwise be handed a document with no name.
#[tokio::test]
async fn an_attachment_tells_the_model_which_file_it_was() {
    let dir = scratch("attach-named");
    let path = dir.join("diagram.png");
    std::fs::write(&path, PNG).expect("written");

    let mut harness = Harness::new([ModelResponse::text("looking")]);
    harness.send(&format!("/attach {}", path.display())).await;

    let request = harness
        .app
        .kernel
        .preview_request()
        .expect("a request with an attachment in it");
    let carried = request
        .messages
        .iter()
        .filter_map(|message| message.content.as_ref())
        .any(|content| content.to_text().contains("diagram.png"));

    assert!(
        carried,
        "the name is not in the request: {:?}",
        request.messages
    );
}

/// A file and a question about it are one thing to type, and go out as one request.
///
/// note: which is the ordinary way to want this. Attaching and then asking works too - the file
/// is in the context either way - but it is two lines and two enters for what a person doing it
/// in any other chat does once.
#[tokio::test]
async fn a_file_and_the_question_about_it_go_in_together() {
    let dir = scratch("attach-and-ask");
    let path = dir.join("notes.md");
    std::fs::write(&path, "the cache was cold.").expect("written");

    let mut harness = Harness::new([ModelResponse::text("it says the cache was cold")]);
    harness
        .send(&format!("/attach {} what happened?", path.display()))
        .await;
    harness.settle().await;

    let items = harness.app.kernel.items();
    assert_eq!(
        items[0].content.as_text(),
        Some("the cache was cold."),
        "the file goes in first, so the question is asked about something already there"
    );
    assert_eq!(items[1].content.as_text(), Some("what happened?"));

    // one turn, and the file was in the request that started it
    let screen = harness.flat();
    assert!(
        screen.contains("it says the cache was cold"),
        "the question should have been sent: {screen}"
    );
}

/// A path with a space in it is a path, not a path and a question.
#[tokio::test]
async fn a_path_with_a_space_is_still_a_path() {
    let dir = scratch("attach-spaces");
    let path = dir.join("my notes.md");
    std::fs::write(&path, "written down").expect("written");

    let mut harness = Harness::new([ModelResponse::text("unreached")]);
    harness.send(&format!("/attach {}", path.display())).await;

    let items = harness.app.kernel.items();
    assert_eq!(
        items.last().expect("the attachment").content.as_text(),
        Some("written down"),
        "splitting on the first space would have looked for a file called `my`"
    );
    assert!(!harness.app.busy, "nothing was asked, so nothing was sent");
}

/// A file this program has no media type for and cannot read as text is refused, rather than
/// guessed at.
///
/// note: the refusal is the feature. A media type is a claim about what the bytes are, and one
/// invented here would build a request the model cannot read and an error message from the
/// endpoint about a shape rather than about a file.
#[tokio::test]
async fn bytes_with_no_media_type_are_refused_and_the_context_is_unchanged() {
    let dir = scratch("attach-unknown");
    let path = dir.join("core.dump");
    std::fs::write(&path, PNG).expect("written");

    let mut harness = Harness::new([ModelResponse::text("unreached")]);
    let before = harness.app.kernel.items().len();
    harness.send(&format!("/attach {}", path.display())).await;

    let screen = harness.screen();
    assert!(
        screen.contains("could not read"),
        "the refusal should name the file and what went wrong: {screen}"
    );
    assert_eq!(
        harness.app.kernel.items().len(),
        before,
        "a refused attachment must not leave half of itself in the context"
    );
}

/// Nothing is attached in the middle of a turn, for the reason a message is not sent into one.
#[tokio::test]
async fn an_attachment_waits_for_the_turn_to_end() {
    let dir = scratch("attach-busy");
    let path = dir.join("notes.md");
    std::fs::write(&path, "nothing much").expect("written");

    let mut harness = Harness::new([ModelResponse::text("thinking")]);
    harness.send("go").await;
    // mid-turn: the answer has not been recorded yet
    harness.send(&format!("/attach {}", path.display())).await;

    let screen = harness.screen();
    assert!(
        screen.contains("not while a turn is running"),
        "an item pushed now lands between a call and its result: {screen}"
    );
}
