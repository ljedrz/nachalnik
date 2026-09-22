//! `context search`: finding text in what is being carried, the archive included, without
//! paying to carry it again.

use crate::{agent, answered, answers_from, one_turn};
use nachalnik::{ContextItem, ContextKind, ContextState, test::call};
use serde_json::json;
use std::sync::Arc;

/// The count and the price come before any line, and the archive is searchable at last.
///
/// note: what `look` cannot do. An archived item is kept in full and never sent, and reading one
/// back copies it into the context - so a session that had put eleven megabytes away could not
/// look inside any of it without undoing the saving it had just made. This is the read that does
/// not cost what carrying it costs, and the header is what keeps it that way.
#[tokio::test]
async fn search_prices_the_matches_before_it_shows_one_and_reaches_the_archive() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "landlock" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "search", "text": "landlock", "take": 1 }),
        ),
    ]));

    let put_away = kernel.push(ContextItem::file(
        "notes.md",
        "the sandbox refuses a connect under Landlock\nand has no UDP right at all\nunrelated line",
    ));
    kernel.push(ContextItem::user("what did we say about the sandbox?"));
    kernel.set_state(
        [put_away],
        ContextState::Archived,
        Some("done with it".into()),
    );

    let before = kernel.budget().used();
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    let (counted, shown) = (&said[0], &said[1]);

    // the count, the price and where - and not one of the lines
    assert!(counted.starts_with("1 line(s) say `landlock`"), "{counted}");
    assert!(counted.contains("tokens if you take them all"), "{counted}");
    assert!(
        !counted.contains("refuses a connect"),
        "a bare search hands back the count, not the lines: {counted}"
    );
    // case ignored, and an archived item is searched rather than skipped
    assert!(counted.contains("archived"), "{counted}");

    // asked for, the line arrives
    assert!(
        shown.contains("refuses a connect under Landlock"),
        "{shown}"
    );

    // and the item is exactly where it was: searching is not a way to pay for something
    let item = kernel.item(put_away).expect("still there");
    assert_eq!(item.state, ContextState::Archived);
    assert!(
        kernel.budget().used() > before,
        "the answers themselves are items and cost what they say"
    );
    assert!(
        !kernel.project().included.contains(&put_away),
        "what was archived is still out of the request: searching it restored nothing"
    );
}

/// A search that finds nothing says what it looked at, rather than only that it found nothing.
#[tokio::test]
async fn a_search_with_no_matches_says_what_it_searched() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "seccomp" }),
        ),
        call("c2", "context", json!({ "action": "search" })),
    ]));

    kernel.push(ContextItem::file("notes.md", "Landlock, and nothing else"));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert!(
        said[0].contains("no line of your context says `seccomp`"),
        "{}",
        said[0]
    );
    assert!(
        said[0].contains("Case was ignored"),
        "a nil result says how it looked, so it is not read as a fact about the context: {}",
        said[0]
    );
    assert!(
        said[0].contains("archived and excluded items were searched"),
        "{}",
        said[0]
    );
    // and a search with nothing to search for is a mistake to correct
    assert!(said[1].contains("needs the `text`"), "{}", said[1]);
}

/// Narrowed to some items, it looks only in those and says so.
#[tokio::test]
async fn search_can_be_held_to_the_items_it_was_given() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "landlock", "ids": [2] }),
    )]));

    kernel.push(ContextItem::file("a.md", "Landlock is here"));
    kernel.push(ContextItem::file("b.md", "and Landlock is here too"));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.starts_with("1 line(s)"), "{said}");
    assert!(said.contains("looking only in 2"), "{said}");
}

/// `search`'s `take` is read the way `log`'s is, rather than by a bare `as_u64` that swallows it.
///
/// note: the same word, two tools apart, meaning two things. `log` has held `take` to a number
/// since it was written - a word is refused by name, because the wrong answer to give is an empty
/// result that reads as an empty log - and `search` read it with `as_u64().map(..)`, so everything
/// that is not a positive integer became `None`, which is the summary, which is what leaving
/// `take` out does. A model that asked for three lines got a count and nothing saying its argument
/// had not been read. `take: 0` was worse than swallowed: it reached `0.min(len)` and printed
/// `the first 0; 1 more match and are not here:` - a heading, a colon, and nothing under it.
#[tokio::test]
async fn a_take_a_search_cannot_read_is_refused_rather_than_ignored() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        // a coherent request for no lines, which is the count and the price - what a call with no
        // `take` gets, and not a heading over an empty list
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "landlock", "take": 0 }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "search", "text": "landlock", "take": -3 }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "search", "text": "landlock", "take": "soon" }),
        ),
        // a numeric string is the number, which costs nothing and saves a turn - `log`'s rule
        call(
            "c4",
            "context",
            json!({ "action": "search", "text": "landlock", "take": "1" }),
        ),
    ]));

    kernel.push(ContextItem::file("a.md", "Landlock is here"));
    kernel.push(ContextItem::file("b.md", "and Landlock is here too"));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert!(said[0].starts_with("2 line(s)"), "{}", said[0]);
    assert!(
        said[0].contains("`take` shows that many"),
        "no lines asked for is the summary: {}",
        said[0]
    );
    assert!(
        !said[0].contains("the first 0"),
        "and never a heading with nothing under it: {}",
        said[0]
    );
    for said in &said[1..3] {
        assert!(
            said.contains("`take` is a whole number"),
            "an argument that cannot be read is named, not dropped: {said}"
        );
        assert!(
            said.contains("Nothing was read"),
            "and says it did nothing, so it cannot be read as a result: {said}"
        );
    }
    assert!(said[3].contains("the first 1"), "{}", said[3]);
}

/// An `ids` that names nothing is said to name nothing, rather than counted as something looked at.
///
/// note: found by printing what the tool answers. `ids: [99]` reported "0 line(s) ... and 1
/// item(s) were looked at" - the count was the length of `ids` rather than the number of items
/// the loop actually read, so a search narrowed to an item that does not exist claimed to have
/// searched it and found nothing. That is a sentence about the context that is false in the one
/// direction a search must never be wrong in: the model is left believing the item is there. Its
/// sibling has always answered `[99] there is no such item`, and there is no reading on which
/// `look` should be the honest one of the two.
#[tokio::test]
async fn a_search_says_which_of_the_ids_it_was_given_name_nothing() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "landlock", "ids": [99] }),
        ),
        // and beside a real one, where the answer is not empty and the missing id could pass
        // unnoticed behind what was found
        call(
            "c2",
            "context",
            json!({ "action": "search", "text": "landlock", "ids": [1, 99] }),
        ),
    ]));

    kernel.push(ContextItem::file("a.md", "Landlock is here"));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert!(
        said[0].contains("0 item(s) were looked at"),
        "nothing was looked at, because the only id named nothing: {}",
        said[0]
    );
    assert!(
        said[0].contains("There is no item 99"),
        "and the reason the search was empty is the fact worth having: {}",
        said[0]
    );
    assert!(said[1].starts_with("1 line(s)"), "{}", said[1]);
    assert!(
        said[1].contains("There is no item 99"),
        "a find does not excuse a missing id: {}",
        said[1]
    );
}

/// What compaction elided out of the request is still findable, and still costs nothing.
///
/// note: the loose end the design set out to close, and the only path nothing else here covers.
/// Eliding is done by the *projector* - the item keeps every byte and the request gets a marker -
/// so the content a compactor moved out is exactly the content nothing could look at. Reading it
/// back with `look` would put the whole item in the request again, which is the saving undone;
/// this finds the line and leaves the marker where it is.
#[tokio::test]
async fn search_reaches_what_compaction_elided_out_of_the_request() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "purple-heron", "take": 5 }),
    )]));

    let big = kernel.push(ContextItem::file(
        "big.txt",
        "noise noise\nthe magic phrase is PURPLE-HERON-42\nmore noise",
    ));
    kernel.push(ContextItem::user("find it"));
    // exactly what a compactor's plan does to an item: a state change, and nothing else
    kernel.set_state(
        [big],
        ContextState::Elided,
        Some("compacted to make room".into()),
    );

    let before = kernel.budget().used();
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("1 line(s) say `purple-heron`"), "{said}");
    // the state is on the row, so the answer says what it reached into
    assert!(said.contains("elided"), "{said}");
    assert!(
        said.contains("the magic phrase is PURPLE-HERON-42"),
        "the line comes back whole: {said}"
    );

    // and the request still carries the marker rather than the file
    let sent = kernel
        .preview_request()
        .expect("there is a request")
        .messages
        .iter()
        .filter_map(|m| m.content.as_ref().map(|c| c.to_text().into_owned()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sent.contains("[..."), "the marker is what goes: {sent}");
    // note: the assertion is about the *item*, not about the word. The search answer is itself an
    // item now and it carries the line it matched, so the phrase is in the request - once,
    // because it was asked for. What is not there is the rest of the file, which is the saving
    // the elision made and the thing a read-it-back would have undone.
    assert!(
        !sent.contains("noise noise") && !sent.contains("more noise"),
        "the elided item is still going as a marker, not as its content: {sent}"
    );
    assert_eq!(
        sent.matches("PURPLE-HERON").count(),
        1,
        "the line came back once, in the answer that was asked for: {sent}"
    );
    assert_eq!(
        kernel.item(big).expect("still there").state,
        ContextState::Elided
    );
    // the answer costs what it says and the item costs what it cost
    assert!(kernel.budget().used() > before);
}

/// The edges nobody types on purpose: multi-byte text, a picture, and absurd arguments.
///
/// note: `search` trims a long matching line to a window around the match, which is index
/// arithmetic over text somebody else wrote - so it is held to CJK, to emoji, and to a match at
/// the very end of a line, where an off-by-one is a panic rather than a wrong answer. A panic in a
/// tool takes the turn with it.
#[tokio::test]
async fn the_edges_of_a_search_and_a_filter_do_not_panic() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "needle", "take": 9 }),
        ),
        call(
            "c2",
            "log",
            json!({ "action": "read", "take": 99_999_999u64 }),
        ),
        call("c3", "log", json!({ "action": "read", "take": -3 })),
        call(
            "c4",
            "log",
            json!({ "action": "read", "since": 99_999_999u64 }),
        ),
        call("c5", "context", json!({ "action": "search", "text": "" })),
    ]));

    // a match at the very end of a line long enough to be windowed, in three-byte characters
    kernel.push(ContextItem::file(
        "cjk.txt",
        format!("{}NEEDLE", "日本語テスト ".repeat(40)),
    ));
    kernel.push(ContextItem::file("emoji.txt", "🦀🦀🦀 NEEDLE 🦀🦀🦀"));
    // a character whose lowercase is a different length, right before the match: the offset the
    // search finds is in the lowercased line, and is not a boundary in the line as written
    kernel.push(ContextItem::file(
        "kelvin.txt",
        format!("{}\u{212A}NEEDLE", "x".repeat(120)),
    ));
    kernel.push(ContextItem::file(
        "odd.txt",
        "a [bracket] and a (paren) and a \\ backslash NEEDLE",
    ));
    kernel.push(ContextItem::file("empty.txt", ""));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context", "log"]);
    let found = &said[0];
    assert!(found.contains("4 line(s) say `needle`"), "{found}");
    // the window kept the match and said it had trimmed the front
    assert!(found.contains("…"), "{found}");
    assert!(found.contains("NEEDLE 🦀🦀🦀"), "{found}");
    assert!(found.contains("backslash NEEDLE"), "{found}");

    // a `take` past the end is the end; a negative one is not a count at all
    assert!(said[1].contains("Showing"), "{}", said[1]);
    assert!(said[2].contains("whole number"), "{}", said[2]);
    // a `since` past the end is a real zero beside a total that is not one
    assert!(said[3].contains("0 match since:"), "{}", said[3]);
    assert!(said[4].contains("needs the `text`"), "{}", said[4]);
}

/// Searching finds a picture by the sentence that stands in for it, and does not read its bytes.
#[tokio::test]
async fn a_search_finds_a_blob_by_what_names_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "image/png", "take": 3 }),
    )]));
    kernel.push(ContextItem::new(
        ContextKind::Reference,
        "user",
        "pic.png",
        nachalnik::Content::Blob(Arc::new(nachalnik::Blob::new("image/png", "AAAABBBB"))),
    ));
    kernel.push(ContextItem::user("what have I given you?"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("1 line(s)"), "{said}");
    assert!(said.contains("pic.png"), "{said}");
    // the standing-in sentence, not the payload: nothing here reads a picture
    assert!(!said.contains("AAAABBBB"), "{said}");
}
