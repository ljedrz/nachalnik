//! `context` changing itself: the moves, what each one leaves behind and says it cost, the
//! notes, and walking any of it back.

use crate::{
    agent, all_answers, answered, answers_from, branch, call_of, offers, one_turn, tokens_in,
};
use nachalnik::{ContextItem, ContextKind, ContextState, ModelResponse, ToolCallId, test::call};
use serde_json::json;

#[tokio::test]
async fn hiding_an_item_says_how_to_get_it_back_and_takes_any_word_for_it() {
    // the failure this closes: a session elided twenty-two items, then spent two calls guessing
    // at how to put them back and gave up. The moment worth saying so is the one where something
    // has just been hidden - and the sentence has to name the spelling that works: it said
    // `state: "restore"` from when the four moves were one argument, and an argument nothing
    // reads is refused by name, so following the instruction cost the call it was there to save
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({"action": "elide", "ids": [1], "reason": "done with it"}),
        ),
        // the instruction the first answer gives, followed to the letter
        call(
            "c2",
            "context",
            json!({"action": "restore", "ids": [1], "reason": "wanted it after all"}),
        ),
    ]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert_eq!(said.len(), 2, "{said:?}");

    assert!(said[0].contains("now elided"), "{}", said[0]);
    assert!(
        said[0].contains("`action: \"restore\"`"),
        "the way back is on the line, spelled the way it is sent: {}",
        said[0]
    );
    assert!(
        said[0].contains("undo"),
        "and so is the bigger hammer: {}",
        said[0]
    );

    // and the call that followed it did what the sentence said it would
    assert_eq!(kernel.items()[0].state, ContextState::Active, "{}", said[1]);
    assert!(
        !said[1].contains("is not an argument") && !said[1].contains("there is no"),
        "the instruction was refused: {}",
        said[1]
    );

    // and putting something back does not then advertise a way back from that
    assert!(!said[1].contains("back:"), "{}", said[1]);
}

#[tokio::test]
async fn eliding_something_small_says_that_it_cost_more_than_it_saved() {
    // an elided item leaves a marker carrying the reason given for eliding it, so on a short item
    // the marker is the more expensive of the two. A live session elided twenty-two of them and
    // added 162 tokens doing it; both numbers were on the screen and it did not notice
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "elide",
            "ids": [1],
            "reason": "a reason long enough to outweigh the four words it is replacing, which is                        the ordinary case for a short item rather than a contrived one",
        }),
    )]));

    kernel.push(ContextItem::user("what does it do?"));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("more than before, not less"), "{said}");
    assert!(said.contains("marker"), "and why: {said}");
}

/// And the reason it gives is the reason for *that* change. One sentence used to serve every
/// action, so writing a note - which grows the request because that is what a note is for - was
/// told its ten extra tokens were a marker left by an elision it had not performed.
#[tokio::test]
async fn a_note_that_makes_the_request_bigger_is_not_blamed_on_an_elision() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "note",
            "label": "code word",
            "content": "the code word is PELICAN",
            "reason": "worth keeping",
        }),
    )]));

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("the next request is now"), "{said}");
    assert!(
        !said.contains("marker"),
        "no marker was left anywhere: {said}"
    );
    assert!(
        !said.contains("not less"),
        "and a note costing what it says is not a surprise to account for: {said}"
    );
}

/// Content coming back into the request gets its own account of why the figure went up, rather
/// than the elision one or none at all.
#[tokio::test]
async fn restoring_something_says_the_growth_is_the_content_itself() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "exclude", "ids": [1], "reason": "not needed for now" }),
        )]),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "context",
            json!({ "action": "restore", "ids": [1], "reason": "needed after all" }),
        )]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user(
        "a message long enough that leaving it out and putting it back moves the figure by more \
         than nothing at all, which is the whole of what this is checking",
    ));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("more than before"), "{said}");
    assert!(said.contains("costs what it says"), "and why: {said}");
    assert!(!said.contains("marker"), "nothing was elided: {said}");
}

/// And a change that makes the request *smaller* says so in words, rather than leaving two numbers
/// to be subtracted. Growth was accounted for and a drop was not, on the reasoning that a drop is
/// what the caller asked for.
///
/// note: a live session pruned three times running, was told `~9,679, from ~10,273`, then `~9,810,
/// from ~9,840`, then `~10,521, from ~11,137` - three drops - and called all three of them growth,
/// because it was measuring each against a figure it remembered from a `budget` several turns back
/// rather than against the `from ~` in front of it. It concluded that pruning adds cost and acted
/// on the conclusion with `undo steps: 6`, which cost it 8,619 tokens. Hence the second half of the
/// sentence as well as the first: where the number it is measured against comes from.
#[tokio::test]
async fn pruning_something_says_the_figure_went_down_and_what_it_went_down_from() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "exclude", "ids": [1], "reason": "not needed for now" }),
    )]));

    kernel.push(ContextItem::user(
        "a message long enough that leaving it out moves the figure by more than nothing at all, \
         which is the whole of what this is checking",
    ));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains("less than before"),
        "which way it went is said rather than left to be worked out: {said}"
    );
    assert!(
        said.contains("not what an earlier `budget` said"),
        "and what `before` is measured from, which is the trap: {said}"
    );
    assert!(
        !said.contains("more than before"),
        "and it did not go up: {said}"
    );
}

/// And a change that moves the figure not at all says *that*, rather than printing one number
/// twice and leaving it to be noticed.
///
/// note: the third arm of the same failure. A pin changes what compaction may take and not what
/// the request carries, so the two figures are identical - and a live session read `now ~6,097
/// tokens, from ~6,097` as "huh, pinning increased the cost slightly?". Two numbers that are not
/// even different were still read as a rise, which says the arithmetic was never what was being
/// done. The sentence also rules out the other reading of an unmoved figure, which is a change
/// that silently did not take.
#[tokio::test]
async fn a_change_that_moves_the_figure_not_at_all_says_that_it_did_not() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "pin", "ids": [1], "reason": "worth keeping through a compaction" }),
    )]));

    kernel.push(ContextItem::user(
        "a message that pinning does not move in or out of anything",
    ));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains("the same figure as before"),
        "an unmoved figure is said in words: {said}"
    );
    assert!(
        said.contains("rather than a change that did not take"),
        "and it is not read as a pin that failed: {said}"
    );
    assert!(
        !said.contains("more than before") && !said.contains("less than before"),
        "it went neither way: {said}"
    );
}

/// A move needs to know which items, and `label` is not how it is said - but it is a way somebody
/// could reasonably think it was, because `label` is in the same schema. So the refusal is the
/// spelling of what they meant rather than a restatement of the arguments.
#[tokio::test]
async fn a_move_given_a_label_instead_of_ids_is_told_how_to_say_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "elide",
            "label": "secrets.txt",
            "reason": "of no further interest",
        }),
    )]));

    kernel.push(ContextItem::file("secrets.txt", "nothing much"));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains(r#"select: "label:secrets.txt""#),
        "the exact thing to say next: {said}"
    );
    assert!(
        kernel.items().iter().all(|item| !item.state.is_elided()),
        "and nothing was moved on a guess"
    );
}

#[tokio::test]
async fn context_prunes_what_is_the_models_and_refuses_what_is_not() {
    let (kernel, provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "exclude",
            "ids": [1, 2, 3],
            "reason": "I am done with this",
        }),
    )]));

    kernel.push(ContextItem::file("keep.rs", "keep me").pinned());
    kernel.push(ContextItem::system("be brief"));
    kernel.push(ContextItem::file("junk.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("a pin is a promise"), "{said}");
    assert!(said.contains("system instruction"), "{said}");
    assert!(said.contains("1 item(s) are now excluded: 3"), "{said}");

    let items = kernel.items();
    assert_eq!(items[0].state, ContextState::Pinned);
    assert_eq!(items[1].state, ContextState::Active);
    assert_eq!(items[2].state, ContextState::Excluded);
    assert_eq!(items[2].note.as_deref(), Some("I am done with this"));

    // the reason it gave is the reason the request reports, and the item really is gone from it
    let after = provider.requests().last().unwrap().clone();
    assert!(
        !after
            .messages
            .iter()
            .filter_map(|message| message.content.as_ref())
            .any(|content| content.to_text().contains("0000"))
    );
    // it is still listed, though: nothing was destroyed
    assert_eq!(kernel.items().len(), items.len());
}

#[tokio::test]
async fn context_may_unpin_only_what_it_pinned_itself() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "pin", "ids": [1], "reason": "I need this" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "restore", "ids": [1], "reason": "no I do not" }),
        ),
    ]));

    kernel.push(ContextItem::file("maybe.rs", "..."));
    kernel.push(ContextItem::user("think about it"));

    kernel.turn().await.expect("the turn failed");

    let answers = all_answers(&kernel);
    assert!(
        answers[0].contains("1 item(s) are now pinned"),
        "{answers:?}"
    );
    assert!(
        answers[1].contains("1 item(s) are now active"),
        "{answers:?}"
    );
    assert!(!answers[1].contains("a pin is a promise"), "{answers:?}");
    assert_eq!(kernel.items()[0].state, ContextState::Active);
}

/// And a pin the person has since made their own is theirs, even on an item the model once pinned.
///
/// note: what the model had pinned was kept as a list of identifiers written only by its own
/// moves, so the person taking the pin off and putting one back left the item on the list - and
/// the model's next `restore` took the person's pin away.
#[tokio::test]
async fn a_pin_the_person_made_again_is_the_persons() {
    let mut script = one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "pin", "ids": [1], "reason": "I need this" }),
    )]);
    script.extend(one_turn(vec![call(
        "c2",
        "context",
        json!({ "action": "restore", "ids": [1], "reason": "no I do not" }),
    )]));
    let (kernel, _provider, _anchor) = agent(script);

    let file = kernel.push(ContextItem::file("maybe.rs", "..."));
    kernel.push(ContextItem::user("think about it"));
    kernel.turn().await.expect("the first turn failed");
    assert_eq!(kernel.item(file).unwrap().state, ContextState::Pinned);

    // the person takes it off and puts their own on, the way `p` on the context tab does
    kernel.set_state([file], ContextState::Active, None);
    kernel.set_state([file], ContextState::Pinned, None);

    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.expect("the second turn failed");

    assert!(
        answered(&kernel).contains("a pin is a promise"),
        "{}",
        answered(&kernel)
    );
    assert_eq!(kernel.item(file).unwrap().state, ContextState::Pinned);
}

#[tokio::test]
async fn context_will_not_touch_the_turn_it_is_speaking_in() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "exclude", "ids": [2], "reason": "on reflection" }),
    )]));

    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    // item 2 is the assistant turn carrying the call: excluding it would take the call down with
    // it, and the answer to the call with it
    let said = answered(&kernel);
    assert!(said.contains("the assistant turn this very call"), "{said}");
    assert_eq!(kernel.items()[1].state, ContextState::Active);
}

#[tokio::test]
async fn revise_rewrites_an_item_and_says_who_did_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "revise",
            "ids": [1],
            "content": "the parser is in src/parse.rs, not src/parser.rs",
            "reason": "I wrote down the wrong path",
        }),
    )]));

    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let item = kernel.item(nachalnik::ContextId(1)).unwrap();
    assert_eq!(
        item.content.to_text(),
        "the parser is in src/parse.rs, not src/parser.rs"
    );
    assert_eq!(item.meta["revised"]["by"], "context");
    assert_eq!(
        item.meta["revised"]["reason"],
        "I wrote down the wrong path"
    );

    // and the whole of what it said before is on the record, because nothing else could recover it
    assert!(kernel.history().iter().any(|record| matches!(
        &record.event,
        nachalnik::Event::ContextReplaced { was, .. }
            if was.to_text().contains("src/parser.rs")
    )));
}

/// `revise` refuses what text cannot stand for, which is what a person's edit refuses: a picture,
/// and a turn whose calls are not in the words it would be replacing.
///
/// note: the model's way in skipped the question the person's way asks, so a picture could be
/// written over with a sentence, and a turn's calls replaced by one - and the calls' results,
/// orphaned, were quietly repaired out of the request after it.
#[tokio::test]
async fn revise_refuses_what_text_cannot_stand_for() {
    let revise = |id: &str, item: u64| {
        call(
            id,
            "context",
            json!({ "action": "revise", "ids": [item], "content": "a sentence", "reason": "shorter" }),
        )
    };
    let (kernel, _provider, _anchor) = agent(one_turn(vec![revise("c1", 1), revise("c2", 2)]));

    kernel.push(ContextItem::new(
        ContextKind::Reference,
        "user",
        "pic.png",
        nachalnik::Content::Blob(std::sync::Arc::new(nachalnik::Blob::new(
            "image/png",
            "AAAABBBB",
        ))),
    ));
    kernel.push(ContextItem::assistant(
        nachalnik::Content::text(""),
        vec![nachalnik::ToolCall::new(
            "earlier",
            "look",
            std::sync::Arc::new(json!({})),
        )],
    ));
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn failed");

    assert!(
        kernel
            .item(nachalnik::ContextId(1))
            .unwrap()
            .content
            .as_blob()
            .is_some(),
        "the picture was written over"
    );
    assert!(
        kernel
            .item(nachalnik::ContextId(2))
            .unwrap()
            .content
            .to_text()
            .is_empty(),
        "a sentence went onto a turn that is a call and nothing else"
    );
    let said = answers_from(&kernel, &["context"]);
    assert!(said[0].contains("picture"), "{}", said[0]);
    assert!(said[1].contains("tool call"), "{}", said[1]);
}

#[tokio::test]
async fn undo_walks_back_this_tools_own_changes_and_nothing_else() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "exclude", "ids": [1], "reason": "too long" }),
        ),
        call(
            "c2",
            "context",
            json!({
                "action": "revise",
                "ids": [2],
                "content": "shorter",
                "reason": "it was verbose",
            }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "undo", "steps": 5, "reason": "I was wrong about both" }),
        ),
    ]));

    // the person's own decision, made before the turn: it must survive an undo that is not theirs
    let theirs = kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::memory("notes", "a long note"));
    kernel.push(ContextItem::user("tidy up"));
    kernel.set_state([theirs], ContextState::Elided, Some("their call".into()));

    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    let walked = said.last().unwrap();
    assert!(
        walked.contains("walked 2 of your own change(s) back"),
        "{walked}"
    );
    assert!(
        walked.contains("0 change(s) of yours can still be undone"),
        "{walked}"
    );

    // both of its own changes are gone, including the note it wrote
    let big = kernel.item(nachalnik::ContextId(1)).unwrap();
    assert_eq!(big.state, ContextState::Elided);
    assert_eq!(big.note.as_deref(), Some("their call"));
    let notes = kernel.item(nachalnik::ContextId(2)).unwrap();
    assert_eq!(notes.content.to_text(), "a long note");
    assert!(notes.meta["revised"].is_null());
}

/// A pin the person puts on afterwards survives the model's undo, and the answer says it was left.
///
/// note: the half the test above does not cover. That one makes the person's decision *before* the
/// model's change, where the journal never held the item; this one makes it in between, where the
/// journal holds `[1] was active` and walking back used to write that over a pin made since -
/// silently, and against the one promise the word makes. Every other move in this tool asks
/// `protected` first; `undo` went straight to `set_state`.
#[tokio::test]
async fn an_undo_does_not_walk_back_over_a_decision_made_since() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "elide", "ids": [1, 2], "reason": "read them both" }),
        )]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "context",
            json!({ "action": "undo", "reason": "I want them back" }),
        )]),
        ModelResponse::text("done"),
    ]);

    let theirs = kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    let mine = kernel.push(ContextItem::file("other.rs", "1".repeat(400)));
    kernel.push(ContextItem::user("tidy up"));
    kernel.turn().await.expect("the first turn failed");

    // between the two turns, the person pins one of the items the model elided
    kernel.set_state([theirs], ContextState::Pinned, Some("I need this".into()));
    kernel.turn().await.expect("the second turn failed");

    let said = all_answers(&kernel);
    let walked = said.last().unwrap();
    assert_eq!(
        kernel.item(theirs).unwrap().state,
        ContextState::Pinned,
        "a pin the person made was walked back over: {walked}"
    );
    assert!(
        walked.contains("left alone") && walked.contains("a pin is a promise"),
        "and it says what it did not touch: {walked}"
    );
    assert_eq!(
        kernel.item(mine).unwrap().state,
        ContextState::Active,
        "the rest of the change still walked back: {walked}"
    );
}

/// Walking back a move of several items is one undo for the person, not one for each item.
///
/// note: `Undoing::apply` called `set_state` an item at a time, so undoing what this tool reported
/// as one change left three checkpoints on the person's stack - and one operation is one undo.
/// They are grouped by the state they land in now, which is also what the report names: it used to
/// say every item was in the state of the first of them.
///
/// note: counted against the same session moving one item rather than against a number, because
/// what is being claimed is that the size of a move does not reach the person's stack - and a
/// number here would be counting the pushes either session happens to make.
#[tokio::test]
async fn walking_back_one_move_is_one_undo_for_the_person() {
    async fn walked(moved: Vec<u64>) -> usize {
        let (kernel, _provider, _anchor) = agent(vec![
            ModelResponse::tool_calls(vec![call(
                "c1",
                "context",
                json!({ "action": "exclude", "ids": moved, "reason": "these" }),
            )]),
            ModelResponse::text("done"),
            ModelResponse::tool_calls(vec![call(
                "c2",
                "context",
                json!({ "action": "undo", "reason": "put them back" }),
            )]),
            ModelResponse::text("done"),
        ]);

        for name in ["a.rs", "b.rs", "c.rs"] {
            kernel.push(ContextItem::file(name, "0".repeat(400)));
        }
        kernel.push(ContextItem::user("tidy up"));
        kernel.turn().await.expect("the first turn failed");
        kernel.turn().await.expect("the second turn failed");

        let mut depth = 0;
        while kernel.undo().unwrap() {
            depth += 1;
        }

        depth
    }

    assert_eq!(
        walked(vec![1, 2, 3]).await,
        walked(vec![1]).await,
        "undoing a move of three items cost the person more than undoing a move of one"
    );
}

/// And the report says which state each item went to, rather than the first one's for all of them.
#[tokio::test]
async fn walking_back_says_where_each_item_ended_up() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "elide", "ids": [1, 2], "reason": "both" }),
        )]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "context",
            json!({ "action": "undo", "reason": "back" }),
        )]),
        ModelResponse::text("done"),
    ]);

    let excluded = kernel.push(ContextItem::file("a.rs", "0".repeat(400)));
    kernel.push(ContextItem::file("b.rs", "1".repeat(400)));
    kernel.push(ContextItem::user("tidy up"));
    // one of the two was already out of the request when the move found it, so the way back is
    // two states and the report has two things to say
    kernel.set_state([excluded], ContextState::Excluded, Some("mine".into()));
    kernel.turn().await.expect("the first turn failed");
    kernel.turn().await.expect("the second turn failed");

    let walked = all_answers(&kernel).last().unwrap().clone();
    assert!(walked.contains("1 now excluded"), "{walked}");
    assert!(walked.contains("2 now active"), "{walked}");
}

/// A walk of several steps knows a pin it put back itself is its own, for the steps after it.
///
/// note: which pins are the model's was read once, before the walk, so a step that put the model's
/// own pin back left the next step reading it as the person's - and walking back a `pin` and the
/// `restore` after it stopped halfway, pinned, and called the pin a promise it could not break.
#[tokio::test]
async fn a_walk_that_puts_its_own_pin_back_can_walk_past_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "pin", "ids": [1], "reason": "I need this" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "restore", "ids": [1], "reason": "no I do not" }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "undo", "steps": 2, "reason": "neither" }),
        ),
    ]));

    let file = kernel.push(ContextItem::file("maybe.rs", "..."));
    kernel.push(ContextItem::user("think about it"));
    kernel.turn().await.expect("the turn failed");

    let walked = all_answers(&kernel).last().unwrap().clone();
    assert!(!walked.contains("a pin is a promise"), "{walked}");
    assert!(
        walked.contains("walked 2 of your own change(s) back"),
        "{walked}"
    );
    let item = kernel.item(file).unwrap();
    assert_eq!(item.state, ContextState::Active);
    assert_eq!(item.note, None);
}

/// A number that is not an item number is refused, and the same number twice is one item.
///
/// note: `ids` dropped whatever it could not read, so `[-1]` arrived as no items at all - which is
/// how a call that named none arrives too. `search` then searched the whole context, and a call
/// giving `ids` *and* `select` went through as a `select`, the refusal for naming items twice
/// having found no numbers to object to. A duplicate was counted twice in what a move reported.
///
/// note: an empty `ids` beside a selector is a selector. It names nothing, so nothing is named
/// twice, and a model that fills every optional list with `[]` was refused on every `look` it
/// wrote with a `select`; a list with a number in it is still refused.
#[tokio::test]
async fn an_id_that_is_not_one_is_refused_rather_than_dropped() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "elide", "ids": [-1], "reason": "the first one" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "elide", "ids": [1, 1], "reason": "twice over" }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "look", "ids": [], "select": "all:files" }),
        ),
        call(
            "c4",
            "context",
            json!({ "action": "look", "ids": [1], "select": "all:files" }),
        ),
    ]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    assert!(said[0].contains("not an item number"), "{}", said[0]);
    assert!(said[0].contains("nothing was done"), "{}", said[0]);

    assert!(
        said[1].contains("1 item(s)") || said[1].contains("[1]"),
        "the same id twice is one item: {}",
        said[1]
    );
    assert!(!said[1].contains("2 item(s)"), "{}", said[1]);

    assert!(
        !said[2].contains("nothing was done") && said[2].contains("big.rs"),
        "an empty `ids` names nothing, so the selector is the call: {}",
        said[2]
    );
    assert!(
        said[3].contains("`ids` and `select` in one call"),
        "a number and a selector name items twice: {}",
        said[3]
    );
}

/// A `steps` outside what a walk takes is refused, rather than walking some other number.
///
/// note: it was `as_u64().unwrap_or(1).clamp(1, 64)`, so nought walked one change back, a word
/// walked one back, and a hundred walked sixty-four - each of them a call that did something other
/// than what it said, and the schema advertised none of it.
#[tokio::test]
async fn a_walk_of_no_steps_walks_nothing() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "elide", "ids": [1], "reason": "done with it" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "undo", "steps": 0, "reason": "none of it" }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "undo", "steps": "two", "reason": "a word" }),
        ),
        call(
            "c4",
            "context",
            json!({ "action": "undo", "steps": 100, "reason": "all of it" }),
        ),
    ]));

    let big = kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    for refusal in &said[1..4] {
        assert!(refusal.contains("`steps` is"), "{refusal}");
        assert!(refusal.contains("from 1 to 64"), "{refusal}");
        assert!(!refusal.contains("walked"), "{refusal}");
    }
    assert_eq!(
        kernel.item(big).unwrap().state,
        ContextState::Elided,
        "a refused `steps` walked something back anyway"
    );
}

#[tokio::test]
async fn undo_with_nothing_of_its_own_says_whose_undo_it_is_not() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "undo", "reason": "let me try" }),
    )]));

    let file = kernel.push(ContextItem::file("theirs.rs", "..."));
    kernel.push(ContextItem::user("go"));
    kernel.set_state([file], ContextState::Excluded, Some("their call".into()));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("nothing of yours to walk back"), "{said}");
    assert!(!said.contains("resumed"), "nobody resumed this: {said}");
    // the person's exclusion is exactly where they left it
    assert_eq!(kernel.items()[0].state, ContextState::Excluded);
}

/// After a resume, an `undo` with nothing to walk says the earlier changes were not carried over.
///
/// note: the journal is the process's, so a model that excluded something before a restart and
/// asks to undo it afterwards was told only whose undo this is not - which reads as though the
/// exclusion had been the person's.
#[tokio::test]
async fn undo_after_a_resume_says_the_earlier_changes_were_not_carried_over() {
    use std::sync::Arc;

    let first = nachalnik::Kernel::new(nachalnik::Config::default());
    let read = first.push(ContextItem::file("read.rs", "..."));
    first.set_state([read], ContextState::Excluded, Some("done with it".into()));

    let kernel = nachalnik::Kernel::resume(nachalnik::Config::default(), first.snapshot());
    kernel.set_provider(Arc::new(nachalnik::test::ScriptedProvider::new(one_turn(
        vec![call(
            "c1",
            "context",
            json!({ "action": "undo", "reason": "put it back" }),
        )],
    ))));
    let policy = Arc::new(kamchatka::tools::Careful::new());
    policy.set(
        &kamchatka::tools::Subject::parse("context"),
        nachalnik::Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor =
        kamchatka::introspect::install(&kernel, policy, kamchatka::tools::Limits::default());
    kernel.push(ContextItem::user("undo that"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("nothing of yours to walk back"), "{said}");
    assert!(said.contains("resumed from a snapshot"), "{said}");
    assert!(said.contains("`restore`"), "{said}");
    assert_eq!(kernel.item(read).unwrap().state, ContextState::Excluded);
}

/// A pin the model made is still the model's after a resume, and one the person made is still
/// theirs.
///
/// note: the tool remembered its own pins in memory alone, so a resumed session had every pin the
/// person's - which fails safe, and refused the model an item it had pinned itself. The pin is
/// written into the item's metadata now, which the snapshot carries.
#[tokio::test]
async fn a_pin_the_model_made_is_still_its_own_after_a_resume() {
    use std::sync::Arc;

    let (first, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "pin", "ids": [1], "reason": "the spec, for the rest of the session" }),
    )]));
    let spec = first.push(ContextItem::file("spec.md", "..."));
    let theirs = first.push(ContextItem::file("theirs.md", "..."));
    first.set_state([theirs], ContextState::Pinned, None);
    first.push(ContextItem::user("keep the spec"));
    first.turn().await.expect("the turn failed");
    assert_eq!(first.item(spec).unwrap().state, ContextState::Pinned);

    let kernel = nachalnik::Kernel::resume(nachalnik::Config::default(), first.snapshot());
    kernel.set_provider(Arc::new(nachalnik::test::ScriptedProvider::new(one_turn(
        vec![call(
            "c2",
            "context",
            json!({ "action": "restore", "ids": [spec.0, theirs.0], "reason": "done with both" }),
        )],
    ))));
    let policy = Arc::new(kamchatka::tools::Careful::new());
    policy.set(
        &kamchatka::tools::Subject::parse("context"),
        nachalnik::Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _resumed =
        kamchatka::introspect::install(&kernel, policy, kamchatka::tools::Limits::default());
    kernel.push(ContextItem::user("let them go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert_eq!(
        kernel.item(spec).unwrap().state,
        ContextState::Active,
        "its own pin was refused it: {said}"
    );
    assert_eq!(kernel.item(theirs).unwrap().state, ContextState::Pinned);
    assert!(said.contains("pinned by the person"), "{said}");
}

/// The model's undo leaves an item the person has moved since, and says so; what it moved and
/// nobody touched it still walks back.
///
/// note: the undo put an item back where the model had had it whatever had happened in between, so
/// a person who restored something the model had excluded saw it excluded again by a move of the
/// model's that knew nothing about theirs.
#[tokio::test]
async fn an_undo_leaves_what_the_person_moved_since() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "exclude", "ids": [1, 2], "reason": "not needed" }),
        )]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "context",
            json!({ "action": "undo", "reason": "I was wrong" }),
        )]),
        ModelResponse::text("done"),
    ]);
    let theirs = kernel.push(ContextItem::file("theirs.rs", "..."));
    let untouched = kernel.push(ContextItem::file("untouched.rs", "..."));
    kernel.push(ContextItem::user("tidy up"));
    kernel.turn().await.expect("the turn failed");

    // the person puts one of them back and excludes it again for a reason of their own
    kernel.set_state([theirs], ContextState::Active, None);
    kernel.set_state([theirs], ContextState::Excluded, Some("mine now".into()));
    kernel.push(ContextItem::user("undo that"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert_eq!(
        kernel.item(untouched).unwrap().state,
        ContextState::Active,
        "{said}"
    );
    assert_eq!(kernel.item(theirs).unwrap().state, ContextState::Excluded);
    assert_eq!(
        kernel.item(theirs).unwrap().note.as_deref(),
        Some("mine now")
    );
    assert!(
        said.contains(&format!("[{theirs}] has changed since")),
        "{said}"
    );
}

/// The same for a revision: an item the person rewrote after the model did keeps the person's text.
#[tokio::test]
async fn an_undo_leaves_what_the_person_rewrote_since() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "context",
            json!({ "action": "revise", "ids": [1], "content": "the model's", "reason": "shorter" }),
        )]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "context",
            json!({ "action": "undo", "reason": "put it back" }),
        )]),
        ModelResponse::text("done"),
    ]);
    let note = kernel.push(ContextItem::memory("a note", "the original"));
    kernel.push(ContextItem::user("shorten it"));
    kernel.turn().await.expect("the turn failed");
    assert_eq!(kernel.item(note).unwrap().content.to_text(), "the model's");

    kernel
        .replace(note, nachalnik::Content::text("the person's"))
        .expect("an edit");
    kernel.push(ContextItem::user("undo that"));
    kernel.turn().await.expect("the turn failed");

    assert_eq!(kernel.item(note).unwrap().content.to_text(), "the person's");
    assert!(
        answered(&kernel).contains("has changed since"),
        "{}",
        answered(&kernel)
    );
}

#[tokio::test]
async fn a_reason_is_required_before_anything_changes() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "exclude", "ids": [1] }),
    )]));

    kernel.push(ContextItem::file("junk.rs", "..."));
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn failed");

    assert!(answered(&kernel).contains("`reason` is required"));
    assert_eq!(kernel.items()[0].state, ContextState::Active);
}

/// A note refused for want of a reason says it was not written, and what to call.
///
/// note: the refusal used to be only the reason a reason is asked for, and a model read it as a
/// remark about a note it had written: it went straight on to elide the results the note was
/// written from, and what it had found was gone.
#[tokio::test]
async fn a_note_without_a_reason_says_it_was_not_written() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "note", "content": "the finding" }),
    )]));
    kernel.push(ContextItem::user("write that down"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("nothing was done"), "{said}");
    assert!(said.contains("call `note` again"), "{said}");
    assert!(
        kernel
            .items()
            .iter()
            .all(|item| item.content.to_text() != "the finding"),
        "and nothing was"
    );
}

/// The word for the move is the action, and it is the word the result is read back in.
///
/// note: these five used to be one `prune` action with a `state` argument, which put the word for
/// one of them over all five - `pin` and `restore` included, so "prune to pin it" was the
/// documented way to protect something, and an item you pruned read back as `archived`. Two live
/// models in a row spent a call each asking for `restore` as an action and being told it was a
/// state; they were right and the levels were wrong. The old spelling still works, because
/// accepting a word somebody reached for costs nothing and refusing it costs a turn.
#[tokio::test]
async fn each_move_is_an_action_named_for_what_it_leaves_behind() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "elide", "ids": [1], "reason": "it is enormous" }),
        ),
        // the way it was spelled before, which is still a way to spell it
        call(
            "c2",
            "context",
            json!({ "action": "exclude", "ids": [2], "reason": "and this one" }),
        ),
        // and the way back, which is an action like the rest of them
        call(
            "c3",
            "context",
            json!({ "action": "restore", "ids": [1], "reason": "I want it after all" }),
        ),
    ]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::file("bigger.rs", "1".repeat(400)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(said[0].contains("elided"), "{}", said[0]);
    assert!(said[1].contains("excluded"), "{}", said[1]);
    assert_eq!(kernel.items()[0].state, ContextState::Active, "{}", said[2]);
    assert_eq!(kernel.items()[1].state, ContextState::Excluded);

    // and nothing tells anybody about a `state` argument any more, because there is not one
    let offered = kernel
        .tool_specs()
        .into_iter()
        .find(|spec| spec.id == "context")
        .expect("it is offered");
    let offers = offers(&offered.schema);
    for action in ["elide", "exclude", "pin", "restore"] {
        assert!(
            offers.contains(&action),
            "`{action}` is not offered as an action"
        );
        assert!(
            branch(&offered.schema, action)["properties"]
                .get("state")
                .is_none(),
            "the level that caused this is still in the schema: {}",
            offered.schema
        );
    }
}

#[tokio::test]
async fn a_class_of_items_can_be_pruned_without_naming_each_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({
                "action": "elide",
                "select": "all:tool_results",
                "reason": "I have what I needed from them",
            }),
        ),
        call(
            "c2",
            "context",
            json!({
                "action": "elide",
                "select": "kind:nothing_like_this",
                "reason": "trying it on",
            }),
        ),
    ]));

    kernel.push(ContextItem::assistant(
        "",
        vec![call_of("t1"), call_of("t2")],
    ));
    for id in ["t1", "t2"] {
        kernel.push(ContextItem::tool_result(
            ToolCallId::from(id),
            "shell",
            "0".repeat(500),
            false,
        ));
    }
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn ran");

    let answers = all_answers(&kernel);
    assert!(
        answers[0].contains("2 item(s) are now elided"),
        "{answers:?}"
    );
    for item in kernel.items() {
        if matches!(item.kind, ContextKind::ToolResult { ref tool, .. } if tool == "shell") {
            assert_eq!(item.state, ContextState::Elided);
        }
    }
    // a selector it got wrong is answered with the whole grammar, so the next attempt is an
    // informed one rather than another guess
    assert!(answers[1].contains("is not a selector"), "{answers:?}");
    assert!(answers[1].contains("tool:fs:latest"), "{answers:?}");
    assert!(answers[1].contains("state:excluded"), "{answers:?}");
}

/// A call that says which items twice is refused, rather than half of it being done.
///
/// note: `select` won and `ids` was dropped without a word, which is the shape of failure the
/// wrapper already refuses one level out - arguments inside `call` and beside it. The answer to a
/// call like this one is an ordinary report of forty items moved, and nothing in it mentions the
/// one number that was also asked for, so there is nothing to read that says half the call was
/// never looked at.
#[tokio::test]
async fn naming_the_items_twice_in_one_call_is_refused_rather_than_half_done() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "elide",
            "ids": [2],
            "select": "all:tool_results",
            "reason": "tidying up",
        }),
    )]));

    kernel.push(ContextItem::assistant(
        "",
        vec![call_of("t1"), call_of("t2")],
    ));
    for id in ["t1", "t2"] {
        kernel.push(ContextItem::tool_result(
            ToolCallId::from(id),
            "shell",
            "0".repeat(500),
            false,
        ));
    }
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    assert!(said.contains("nothing was done"), "{said}");
    // both of them quoted back, because which one to drop is the question being asked
    assert!(
        said.contains("`ids`") && said.contains("`select`"),
        "{said}"
    );
    assert!(said.contains("all:tool_results"), "{said}");
    assert!(
        said.contains("select: \\\"2\\\"") || said.contains(r#"select: "2""#),
        "the way to say the numbers as a class is on the line: {said}"
    );
    // and nothing moved: not the id it named, and not the class it named either
    for item in kernel.items() {
        assert_eq!(
            item.state,
            ContextState::Active,
            "[{}] moved on a call that was refused",
            item.id
        );
    }
}

/// A class can be read off before anything moves, and says which of it a move would refuse.
///
/// note: the selector grammar belongs to the half of this tool that changes things, and until
/// `look` took one there was no way to resolve a selector except by using it on something. The
/// preview is the same `Selector::matches` and the same [`protected`] the move consults, so the
/// two cannot disagree about what a class comes to or about which of it is off limits.
#[tokio::test]
async fn a_class_lists_what_it_comes_to_before_a_move_takes_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "look", "select": "files" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "look", "select": "label:nothing-here" }),
        ),
    ]));

    kernel.push(ContextItem::file("src/parser.rs", "fn parse() {}"));
    let big = kernel.push(ContextItem::file("big.rs", "0".repeat(2_000)));
    let theirs = kernel.push(ContextItem::file("keep.rs", "fn keep() {}"));
    kernel.push(ContextItem::user("what is in here?"));
    kernel.set_state([theirs], ContextState::Pinned, Some("mine".into()));

    kernel.turn().await.expect("the turn ran");

    let answers = all_answers(&kernel);
    let listing = &answers[0];
    // what it matched, counted against what there is, so a selector that took more than it meant
    // to reads as one
    assert!(listing.contains("`files` matches 3 of"), "{listing}");
    for named in ["src/parser.rs", "big.rs", "keep.rs"] {
        assert!(listing.contains(named), "{listing}");
    }
    assert!(
        !listing.contains("what is in here?"),
        "the listing is the class and not the context: {listing}"
    );
    // the figures are the class's own, which is the number the decision turns on, with the
    // session's beside it to read them against - so the two are not the same number
    let figures = listing
        .lines()
        .find(|line| line.contains("out of ~"))
        .map(tokens_in)
        .expect("the line with the figures on it");
    assert_eq!(figures.len(), 3, "{listing}");
    assert!(
        figures[0] < figures[2],
        "the class is charged for the whole session: {listing}"
    );
    // the one a move would refuse, marked here rather than found out by moving
    assert!(
        listing.contains("not yours to move: pinned by the person"),
        "{listing}"
    );
    assert!(
        listing.contains("less the 1 marked as not yours"),
        "the closing line counts them: {listing}"
    );
    // and nothing moved, because looking is looking
    assert_eq!(
        kernel.item(big).expect("still there").state,
        ContextState::Active
    );

    // a class that matches nothing is an answer rather than a refusal: the question was which
    // items these are, and the answer is none of them
    assert!(
        answers[1].contains("nothing in your context matches it"),
        "{}",
        answers[1]
    );
}

/// An item asked into the state it is already in did not move, and is not journalled as having.
///
/// note: `StateChange::unchanged` is "already in that state *with that note*", so `pin [2]` on
/// something already pinned, for a new reason, comes back as `changed` - which is true of the note
/// and false of the item. The report read it as a move: `1 item(s) are now pinned: 2`, over figures
/// that had not moved by a token, and it put a step in this tool's journal that `undo` then
/// described as `2 now pinned` about an item that was still pinned. Two calls that restated a
/// pin were two things to walk back and neither walked anything. What actually happened is that
/// the reason was rewritten, so that is what it says now, and the journal holds the moves.
#[tokio::test]
async fn restating_a_state_is_not_a_move_and_is_not_something_to_undo() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "pin", "ids": [2], "reason": "keeping this" }),
        ),
        // the same state, a new reason: the kernel calls this changed, because the note changed
        call(
            "c2",
            "context",
            json!({ "action": "pin", "ids": [2], "reason": "still keeping it" }),
        ),
        // and a call that does both at once, which is the shape that has to stay readable
        call(
            "c3",
            "context",
            json!({ "action": "pin", "ids": [1, 2], "reason": "both now" }),
        ),
        call(
            "c4",
            "context",
            json!({ "action": "undo", "reason": "back" }),
        ),
        call(
            "c5",
            "context",
            json!({ "action": "undo", "reason": "back" }),
        ),
        call(
            "c6",
            "context",
            json!({ "action": "undo", "reason": "back" }),
        ),
    ]));

    kernel.push(ContextItem::user("where is the parser?"));
    kernel.push(ContextItem::memory(
        "notes",
        "the parser is in src/parse.rs",
    ));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert!(
        said[0].starts_with("1 item(s) are now pinned: 2"),
        "{}",
        said[0]
    );

    assert!(
        said[1].starts_with("0 item(s) are now pinned"),
        "nothing moved, and the headline is the place that has to say so: {}",
        said[1]
    );
    assert!(
        said[1].contains("were already pinned and did not move: 2"),
        "{}",
        said[1]
    );
    assert!(
        said[1].contains("the reason, which now reads `still keeping it`"),
        "and what did happen is worth having: {}",
        said[1]
    );

    // one of each in one call: the mover is named as moved and the other as standing still
    assert!(
        said[2].starts_with("1 item(s) are now pinned: 1"),
        "{}",
        said[2]
    );
    assert!(
        said[2].contains("were already pinned and did not move: 2"),
        "{}",
        said[2]
    );

    // and there are two things to walk back, not four: `pin 2` and `pin 1`
    assert!(said[3].contains("1 now active"), "{}", said[3]);
    assert!(said[4].contains("2 now active"), "{}", said[4]);
    assert!(
        said[5].contains("there was nothing of yours to walk back"),
        "a restatement is not a step in the journal: {}",
        said[5]
    );
}

/// Pinning a note costs the person nothing. The undo stack behind the `u` key is theirs, and a
/// note written and *then* pinned was a push and a state change - two checkpoints for one thing
/// the model did, which is the arithmetic `revise` keeps its own account out of the note to avoid.
#[tokio::test]
async fn pinning_a_note_is_not_a_second_thing_to_undo() {
    let written = |pin: bool| async move {
        let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
            "c1",
            "context",
            json!({
                "action": "note", "label": "the plan", "content": "read the tests first",
                "pin": pin, "reason": "so it outlives this turn",
            }),
        )]));
        kernel.push(ContextItem::user("what is your plan?"));
        kernel.turn().await.expect("the turn ran");

        (
            kernel.with_context(|c| c.undo_len()),
            kernel
                .items()
                .into_iter()
                .find(|item| item.label == "the plan")
                .expect("it was written down")
                .state,
        )
    };

    let (loose, loose_state) = written(false).await;
    let (pinned, pinned_state) = written(true).await;

    assert_eq!(loose_state, ContextState::Active);
    assert_eq!(pinned_state, ContextState::Pinned);
    assert_eq!(
        pinned, loose,
        "the pin cost the person an undo of their own"
    );
}

#[tokio::test]
async fn a_note_is_written_down_where_compaction_cannot_reach_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "note",
            "label": "the plan",
            "content": "read the tests first, and do not touch the lexer",
            "pin": true,
            "reason": "so it outlives this turn",
        }),
    )]));

    kernel.push(ContextItem::user("what is your plan?"));
    kernel.turn().await.expect("the turn ran");

    let written = kernel
        .items()
        .into_iter()
        .find(|item| item.label == "the plan")
        .expect("it was written down");

    assert_eq!(written.state, ContextState::Pinned);
    assert!(written.content.to_text().contains("do not touch the lexer"));
    // attributed to whoever wrote it, so "who put these tokens in here?" has an answer
    assert_eq!(written.source, "agent");
    // and the reason is on it, which is what a second state change used to be spent putting there
    assert_eq!(
        written.included_because.as_deref(),
        Some("so it outlives this turn")
    );
    assert_eq!(
        written.included_because.as_deref(),
        Some("so it outlives this turn")
    );

    // and it is one of its own changes, so it can walk it back - which archives it rather than
    // destroying it, like everything else here
    let tool = kernel.tool("context").expect("installed");
    tool.invoke(
        &nachalnik::ToolCall::new("c2", "context", json!({ "action": "undo", "reason": "no" })),
        nachalnik::OutputSink::disconnected(),
    )
    .await
    .expect("the tool answered");

    let written = kernel.item(written.id).expect("still listed");
    assert_eq!(written.state, ContextState::Archived);
    assert!(written.content.to_text().contains("do not touch the lexer"));
}

#[tokio::test]
async fn hiding_everything_while_holding_no_notes_says_what_that_costs() {
    // the failure this closes: a run gathered nineteen thousand tokens across seventeen tool
    // results, said nothing in its own turns, elided all seventeen in one call, and answered from
    // an empty context - inventing all ten answers. `prune` reported the tokens it had given back
    // and nothing about the evidence it had just taken away
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({"action": "elide", "select": "all:tool_results",
                   "reason": "done with these"}),
        ),
        // a note, and then the same wipe again: with something of its own kept, the warning has
        // nothing to warn about
        call(
            "c2",
            "context",
            json!({"action": "note", "label": "q1", "content": "Cargo.lock is 3593 lines",
                   "pin": true, "reason": "keeping the finding"}),
        ),
        call(
            "c3",
            "context",
            json!({"action": "elide", "ids": [2], "reason": "done with it"}),
        ),
    ]));

    kernel.push(ContextItem::tool_result(
        ToolCallId::from("c0"),
        "shell",
        "3593 Cargo.lock",
        false,
    ));
    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert_eq!(said.len(), 3, "{said:?}");

    assert!(
        said[0].contains("no notes"),
        "a wipe with nothing written down should say so: {}",
        said[0]
    );
    assert!(
        said[0].contains("`note`"),
        "and should name the thing that would have helped: {}",
        said[0]
    );
    // note: and it does not overstate the case. It used to say the content "survives only in what
    // you have already said", which was true until `context: search` arrived - an elided item
    // keeps every byte and projects as a marker. A live model read that and told its user the text
    // was gone and no longer retrievable.
    assert!(
        !said[0].contains("survives only in what you have already said"),
        "{}",
        said[0]
    );
    assert!(
        said[0].contains("The text is still in them"),
        "the warning is about what is not being carried, not about destruction: {}",
        said[0]
    );
    assert!(
        said[0].contains("`restore` returns the whole"),
        "{}",
        said[0]
    );
    // once it has kept something of its own, the same move is no longer the same move
    assert!(
        !said[2].contains("no notes"),
        "a note is in context, so there is nothing to warn about: {}",
        said[2]
    );
    assert!(
        said[2].contains("now elided"),
        "and the prune still happened: {}",
        said[2]
    );
}

/// `by: "context"` on an item's metadata means the tool, and nothing else in this program writes it.
///
/// note: the other question the design left open - whether a person editing an item would also
/// record `"context"`, which would have a model reading its own metadata and finding its own tool
/// named as the hand that did it. It would not, and there is no such edit: `Kernel::replace` is
/// reached from `context`'s `revise` and from its `undo`, and from nowhere else in this crate.
/// The `unwrap_or("something")` that suggested otherwise is guarding against metadata written by
/// somebody else's code, which is what a free-form field the kernel never reads is for.
#[tokio::test]
async fn the_only_hand_that_records_itself_as_the_tool_is_the_tool() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "revise",
            "ids": [1],
            "content": "the parser is in src/parse.rs",
            "reason": "I wrote it down wrong",
        }),
    )]));

    let item = kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    // a person rewriting an item goes through the same kernel call and records nothing of its own
    kernel
        .replace(item, "edited by whoever is at the terminal")
        .expect("the item is there");
    assert!(
        kernel.item(item).expect("still there").meta.is_null(),
        "nothing outside the tool attributes an edit to it"
    );

    kernel.turn().await.expect("the turn failed");

    let after = kernel.item(item).expect("still there");
    assert_eq!(after.meta["revised"]["by"], "context");
    // and both rewrites are on the record with what each replaced, so the two hands are told
    // apart by the log even though the item carries only the second
    let replaced: Vec<String> = kernel
        .history()
        .iter()
        .filter_map(|record| match &record.event {
            nachalnik::Event::ContextReplaced { was, .. } => Some(was.to_text().into_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(replaced.len(), 2, "{replaced:?}");
    assert!(replaced[0].contains("src/parser.rs"), "{replaced:?}");
    assert!(
        replaced[1].contains("whoever is at the terminal"),
        "{replaced:?}"
    );
}

/// `note` writes a new item and has no use for an id, so a call that gave it one is refused
/// rather than quietly written anyway.
///
/// note: found live, twice in one evening, by two different models. `note` is one of the nine
/// that change, the `ids` argument says it is for the nine that change, and so `ids` on a `note`
/// reads as *which item to annotate*. Nothing annotates an item here. The call used to succeed,
/// write a free-standing note, and answer with its new number - and the session went on believing
/// the item it had named now carried the words.
///
/// note: `revise` is what it wanted, and is named, on the same argument as the clash sentence
/// below: a refusal that says only what is wrong spends a turn, and one that says what to say
/// instead spends none.
#[tokio::test]
async fn a_note_given_an_id_to_annotate_is_refused_rather_than_written_anyway() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "note",
            "ids": [1],
            "content": "this is the important one",
            "reason": "mark it",
        }),
    )]));
    kernel.push(ContextItem::user("read the notes"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("`note` does not take `ids`"), "{said}");
    assert!(
        said.contains("`content`") && said.contains("`label`"),
        "and what it does take: {said}"
    );
    // `ids` is seven of the twelve, so there is no one operation to send it to and none is
    // named. The first row that takes it would have been `look`, which is a fact about the order
    // of a table in this file and not about `ids`
    assert!(!said.contains("that one is"), "{said}");
    // and no note was written on the way to saying so
    assert!(
        !kernel
            .items()
            .iter()
            .any(|item| matches!(item.kind, ContextKind::Reference) && item.source == "agent"),
        "nothing was written down"
    );
}

/// The mistake a live session actually made, five times over: `note` appends, and a second note
/// under a name already taken is a second item rather than a new value for the first.
#[tokio::test]
async fn a_note_says_when_its_name_is_already_taken_and_what_changes_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({
                "action": "note",
                "content": "experiment_status: in_progress",
                "label": "status",
                "reason": "track it",
            }),
        ),
        call(
            "c2",
            "context",
            json!({
                "action": "note",
                "content": "experiment_status: complete",
                "label": "status",
                "reason": "update it",
            }),
        ),
    ]));
    kernel.push(ContextItem::user("run the experiment"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    // the first took a free name and has nothing to report about it
    assert!(!said[0].contains("that name too"), "{}", said[0]);

    // the second is the one that thought it was assigning to a variable
    assert!(said[1].contains("carries that name too"), "{}", said[1]);
    assert!(
        said[1].contains("`label:status` now names 2"),
        "{}",
        said[1]
    );
    assert!(
        said[1].contains("`revise` rewrites the one you wrote before"),
        "the action for what it was actually trying to do: {}",
        said[1]
    );

    // ... and both are really in the context, which is the thing the sentence is about
    let notes = kernel
        .items()
        .iter()
        .filter(|item| item.label == "status")
        .count();
    assert_eq!(notes, 2, "a note is a new item every time");
}

/// A name nobody chose is not a name that can be taken: two unlabelled notes are both called
/// `note`, and a clash between two names the model never picked is not news.
#[tokio::test]
async fn two_notes_with_no_label_are_not_reported_as_a_clash() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "note", "content": "one", "reason": "why" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "note", "content": "two", "reason": "why" }),
        ),
    ]));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    for said in all_answers(&kernel) {
        assert!(!said.contains("that name too"), "{said}");
    }
}

/// A note that is out of the request costs nothing and contradicts nothing, so a warning about one
/// would be a warning about nothing. What the sentence counts is what still goes into the request.
#[tokio::test]
async fn a_name_freed_by_putting_the_item_away_is_free_again() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "note", "content": "first", "label": "plan", "reason": "why" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "exclude", "select": "label:plan", "reason": "done with it" }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "note", "content": "second", "label": "plan", "reason": "why" }),
        ),
    ]));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    assert!(
        !said[2].contains("that name too"),
        "the first one is out of the request and in nobody's way: {}",
        said[2]
    );
}

/// A few named and the rest counted: the point is that there are others and what to do about it,
/// not a column of numbers.
#[tokio::test]
async fn a_name_taken_many_times_over_names_some_and_counts_the_rest() {
    let notes: Vec<nachalnik::ToolCall> = (0..6)
        .map(|n| {
            call(
                &format!("c{n}"),
                "context",
                json!({
                    "action": "note",
                    "content": format!("step {n}"),
                    "label": "status",
                    "reason": "why",
                }),
            )
        })
        .collect();
    let (kernel, _provider, _anchor) = agent(one_turn(notes));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    let last = said.last().expect("six of them");
    assert!(last.contains("carry that name too"), "{last}");
    assert!(
        last.contains("and 1 more"),
        "four named, the rest counted: {last}"
    );
    assert!(last.contains("`label:status` now names 6"), "{last}");
}

/// A `pin` written in quotes is read, and one that is neither word writes nothing.
///
/// note: `pin` was read with `as_bool`, so `"true"` - which the `fs` tools take - left the note
/// unpinned and nothing said the argument had been passed over.
#[tokio::test]
async fn a_note_pinned_in_quotes_is_pinned() {
    let note = |id, label, pin| {
        call(
            id,
            "context",
            json!({"action": "note", "label": label, "content": "a finding", "pin": pin,
                   "reason": "keeping it"}),
        )
    };
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        note("c1", "quoted", json!("true")),
        note("c2", "worded", json!("yes")),
    ]));

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn ran");

    let find = |label| kernel.items().into_iter().find(|item| item.label == label);
    assert_eq!(find("quoted").expect("written").state, ContextState::Pinned);
    assert!(
        find("worded").is_none(),
        "a `pin` it could not read wrote nothing"
    );
}

/// A revision to what the item already says changes nothing and says so.
#[tokio::test]
async fn revise_to_the_same_words_changes_nothing_and_says_so() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({
            "action": "revise",
            "ids": [1],
            "content": "the parser is in src/parser.rs",
            "reason": "tidying",
        }),
    )]));
    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = &answers_from(&kernel, &["context"])[0];
    assert!(!said.contains("now says something else"), "{said}");
    assert!(
        kernel.item(nachalnik::ContextId(1)).unwrap().meta["revised"].is_null(),
        "an edit that did not happen is credited to the tool"
    );
}

/// A `select` that is not a string is refused, and a `file:` selector that matched nothing says
/// what `file:` names.
///
/// note: both from sessions driven by a model. An object in `select` dropped out as no selector
/// at all, and `look` listed the whole context as though it had never been given. And `file:`
/// with the path of a file the model had read with `fs` matched nothing, because a read is a tool
/// result and `file:` names attached files - five times across three sessions, with nothing in
/// the refusal to say why.
#[tokio::test]
async fn a_select_that_is_not_a_string_is_refused_and_an_empty_file_one_says_why() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "look", "select": { "item": ["1"] } }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "elide", "select": "file:src/main.rs", "reason": "read it" }),
        ),
        call(
            "c3",
            "context",
            json!({ "action": "elide", "select": "label:nothing-here", "reason": "read it" }),
        ),
    ]));

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    assert!(said[0].contains("not a selector"), "{}", said[0]);
    assert!(said[0].contains("nothing was done"), "{}", said[0]);
    assert!(
        !said[0].contains("items ·"),
        "nothing was listed: {}",
        said[0]
    );

    assert!(
        said[1].contains("nothing in your context matches"),
        "{}",
        said[1]
    );
    assert!(said[1].contains("attached"), "{}", said[1]);
    assert!(said[1].contains("tool:fs"), "{}", said[1]);

    assert!(
        said[2].contains("nothing in your context matches"),
        "{}",
        said[2]
    );
    assert!(
        !said[2].contains("attached"),
        "only a `file:` gets the hint: {}",
        said[2]
    );
}

/// An `action` that is not a string is refused as what it is, not as missing.
///
/// note: `the \`action\` argument is required`, said about a call that has one, sends a model to
/// add an argument it already wrote rather than to fix the one it got wrong.
#[tokio::test]
async fn an_action_that_is_not_a_string_is_not_called_missing() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": ["note", "the answer is 42"], "reason": "remember it" }),
        ),
        call("c2", "context", json!({ "reason": "remember it" })),
    ]));

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    assert!(said[0].contains("a list"), "{}", said[0]);
    assert!(said[0].contains("nothing was done"), "{}", said[0]);
    assert!(!said[0].contains("required"), "{}", said[0]);
    assert!(said[1].contains("is required"), "{}", said[1]);
}
