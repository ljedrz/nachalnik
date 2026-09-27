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

/// A `take` above the ceiling is taken as the ceiling, and the answer says which and why.
///
/// note: the tool's whole rule is that it prices the matches before it hands any of them over, and
/// a `take` of "as many as there are" is the one way to ask for it not to be priced. There was no
/// bound: `take: 999999` over a real context returned 13,222 tokens in one tool result, the
/// compactor elided it on the way in, and the model was left holding a marker saying `compacted to
/// make room` - the tool paid for the answer and then had it taken away.
///
/// note: clamped rather than refused, and said. Refusing is what `undo`/`redo` do with a `steps`
/// above their bound, and the reason there is that a walk of sixty-four where a hundred was asked
/// for reads as though the hundred was undone. Here the header is the true count and the true
/// price whatever the `take` was, so a clamped answer is still a true one.
#[tokio::test]
async fn a_take_wider_than_the_ceiling_is_clamped_and_says_so() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "hay", "take": 100_000 }),
    )]));

    // more matching lines than the ceiling, so the clamp and not the matches are what stops it
    for n in 0..200 {
        kernel.push(ContextItem::file(
            format!("f{n}.txt"),
            format!("hay on line one of file {n}\n"),
        ));
    }
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    // the count and the price are of every match, as they are for a `take` that fits
    assert!(said.starts_with("200 line(s) say `hay`"), "{said}");
    // and only the ceiling of them is here
    assert!(said.contains("the first 64;"), "{said}");
    assert!(
        said.contains("`take` is at most 64"),
        "a clamped answer that does not say it was clamped reads as the whole of what matched: \
         {said}"
    );
    assert!(
        said.contains("a narrower `text`"),
        "and says what to do instead, because there is no way to page further: {said}"
    );
    let shown = said.matches("hay on line one of file").count();
    assert_eq!(shown, 64, "the ceiling of them: {said}");
}

/// A `take` under the ceiling is left alone, and the sentence about the ceiling is not in it.
#[tokio::test]
async fn a_take_under_the_ceiling_is_not_announced_as_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "hay", "take": 3 }),
    )]));
    for n in 0..10 {
        kernel.push(ContextItem::file(format!("f{n}.txt"), "hay\n"));
    }
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    assert!(said.contains("the first 3;"), "{said}");
    assert!(
        !said.contains("`take` is at most"),
        "the ceiling is said where it stopped the answer short, and nowhere else: {said}"
    );
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

/// What a turn asked a tool for, and what it thought, are in the context and are found there.
#[tokio::test]
async fn search_finds_the_arguments_a_turn_called_a_tool_with() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "needle.rs" }),
    )]));

    kernel.push(ContextItem::assistant(
        "",
        vec![call(
            "t1",
            "fs",
            json!({ "action": "read", "path": "src/needle.rs" }),
        )],
    ));
    kernel.push(ContextItem::tool_result(
        nachalnik::ToolCallId::from("t1"),
        "fs",
        "fn main() {}",
        false,
    ));
    kernel.push(ContextItem::user("what did you read?"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(!said.starts_with("no line of your context"), "{said}");
}

/// A search does not count the turn making the call, or anything else that turn said or thought.
///
/// note: the call's own arguments are already skipped, because they carry the text being looked
/// for - but the words beside them and the reasoning in front of them were not, so a search for
/// something the model had just said found it in the turn that had said it and reported a match
/// the model had made itself. A search says what the context holds, and the turn asking is the one
/// part of it the model has just written.
///
/// note: and the message that started that turn, which is a context item of its own. Skipping the
/// assistant turn covered the words and the thinking but not the question, so a model searching
/// for a name, an identifier or an error string it had just been handed was told the context
/// already held it - the only place it was being the question the model was in the middle of
/// asking. The turn here is a *later* assistant turn of a turn already under way, so the words to
/// skip are not beside the call either: the search is in the second of two, with a result between
/// them.
///
/// note: the calling turn is put on the context by hand rather than recorded by a provider, so
/// that it carries the words and the thinking a scripted turn would have had to be given.
#[tokio::test]
async fn a_search_does_not_count_the_turn_asking_the_question() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "needle" }),
    )]));

    // an earlier turn of the model's own: what the context holds, and what a later search finds
    kernel.push(ContextItem::assistant(
        "it is on needle",
        vec![call("t1", "fs", json!({ "path": "a.rs" }))],
    ));
    kernel.push(ContextItem::tool_result(
        nachalnik::ToolCallId::from("t1"),
        "fs",
        "fn main() {}",
        false,
    ));

    // the turn that asks, opening with the word it is about to search for
    kernel.push(ContextItem::user(
        "the needle is in src/needle.rs - find it",
    ));
    kernel.push(ContextItem::assistant(
        "reading it, then the needle",
        vec![call("t2", "fs", json!({ "path": "needle.rs" }))],
    ));
    kernel.push(ContextItem::tool_result(
        nachalnik::ToolCallId::from("t2"),
        "fs",
        "the needle is a function",
        false,
    ));
    kernel.push(
        ContextItem::assistant(
            "the needle, then",
            vec![call(
                "c1",
                "context",
                json!({ "action": "search", "text": "needle" }),
            )],
        )
        .with_reasoning(Some(nachalnik::Content::text("searching for the needle"))),
    );
    kernel.push(ContextItem::tool_result(
        nachalnik::ToolCallId::from("c1"),
        "context",
        "unused",
        false,
    ));
    kernel.push(ContextItem::user("again"));

    // the tool is reached by hand now, so that the turn making the call is the one holding `c1`
    let out = call(
        "c1",
        "context",
        json!({ "action": "search", "text": "needle", "take": 9 }),
    );
    let tool = kernel.tool("context").expect("it is installed");
    let answered = nachalnik::Tool::invoke(&*tool, &out, nachalnik::OutputSink::disconnected())
        .await
        .expect("the call was answered");

    let said = answered.content.to_text();
    assert!(
        said.starts_with("1 line(s) say `needle`"),
        "only the earlier turn is the context the search reads: {said}"
    );
    assert!(
        said.contains("it is on needle"),
        "and the match is the one that was already there: {said}"
    );
    assert!(
        !said.contains("is in src/needle.rs - find it")
            && !said.contains("reading it, then the needle")
            && !said.contains("the needle is a function"),
        "a match in the turn that asked is a match the model made itself: {said}"
    );
    assert!(
        !said.contains("the needle, then") && !said.contains("searching for the needle"),
        "and neither is the one beside the call or the thinking in front of it: {said}"
    );
}

/// A word that occurs in nothing but the question is not in the context, whatever the question is.
///
/// note: the plainest form of the same thing, and the one a model reaches for: the second turn
/// hands the model a word and asks it to search for that word. Before the whole turn was skipped
/// the answer came back `1 line(s) say \`qqqzzz\`, in 1 item(s)` - the one item being the message
/// that had just asked. A model that is told its own question is already in its context will
/// reason from that, and the word it was given a moment earlier is exactly the word it is about to
/// be wrong about.
#[tokio::test]
async fn a_word_the_question_itself_carries_is_not_in_the_context() {
    let (kernel, _provider, _anchor) = agent(Vec::new());

    // a context that holds the word nowhere, and a turn that is about to ask for it
    kernel.push(ContextItem::assistant("I am here", Vec::new()));
    kernel.push(ContextItem::user("what did we decide?"));
    kernel.push(ContextItem::user(
        "Now call the context tool once with action \"search\" and text \"qqqzzz\" and nothing \
         else.",
    ));
    kernel.push(ContextItem::assistant(
        "",
        vec![call(
            "c2",
            "context",
            json!({ "action": "search", "text": "qqqzzz" }),
        )],
    ));

    let out = call(
        "c2",
        "context",
        json!({ "action": "search", "text": "qqqzzz" }),
    );
    let tool = kernel.tool("context").expect("it is installed");
    let answered = nachalnik::Tool::invoke(&*tool, &out, nachalnik::OutputSink::disconnected())
        .await
        .expect("the call was answered");

    let said = answered.content.to_text();
    assert!(
        said.starts_with("no line of your context says `qqqzzz`"),
        "the only place that word is, is the question: {said}"
    );
}
