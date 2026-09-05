//! Tests for an assistant turn whose order is part of it.
//!
//! note: the subject is one claim - that a turn recorded as an ordered sequence of blocks
//! survives being recorded, counted, pruned, projected and sent, and that a projector which does
//! *not* speak that dialect flattens it into the conventional three slots and says where that
//! cost something. The order is the information; a test that only checked the parts would be
//! checking everything except the thing under test.

mod common;

use std::sync::Arc;

use nachalnik::{
    Block, BytesPerToken, Config, Content, ContextItem, ContextKind, ContextState, Event, Kernel,
    LinearProjector, ModelResponse, Projector, Role, Snapshot, TokenCounter, ToolCallId,
    test::{ConstTool, call},
};
use serde_json::json;

use common::permissive;

/// A turn that thinks, speaks, asks, speaks again and asks again - the shape a message with one
/// content slot cannot hold.
fn interleaved() -> Vec<Block> {
    vec![
        Block::reasoning(Content::text("the user wants both cities")),
        Block::text(Content::text("Checking Warsaw.")),
        Block::Call(call("c1", "echo", json!({ "city": "Warsaw" }))),
        Block::text(Content::text("And now Krakow.")),
        Block::Call(call("c2", "echo", json!({ "city": "Krakow" }))),
    ]
}

fn kernel_with(blocks: Vec<Block>) -> Kernel {
    let (kernel, _) = permissive([ModelResponse::blocks(blocks), ModelResponse::text("done")]);
    kernel.add_tool(Arc::new(ConstTool::new("echo", "sunny")));
    kernel.push(ContextItem::user("what is the weather?"));

    kernel
}

/// The assistant turn a run produced.
fn turn(kernel: &Kernel) -> Arc<ContextItem> {
    kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
        .expect("a turn was recorded")
}

// ------------------------------------------------------------------------------- the content

#[test]
fn text_is_what_was_said_and_the_length_is_what_it_costs() {
    let content = Content::blocks(interleaved());

    // what the turn said, and only that: the thinking is not something the model uttered, and a
    // provider that put it in a `content` field would be sending it back as if it had
    assert_eq!(content.to_text(), "Checking Warsaw.\nAnd now Krakow.");
    assert!(!content.to_text().contains("the user wants both cities"));

    // what it costs, on the other hand, is all of it
    let whole = content.byte_len();
    assert!(
        whole > content.to_text().len() + "the user wants both cities".len(),
        "{whole} should cover the thinking and both calls' arguments"
    );
    assert_eq!(
        whole,
        interleaved().iter().map(Block::byte_len).sum::<usize>()
    );
}

#[test]
fn a_single_text_block_reads_as_that_text() {
    let content = Content::blocks([Block::text(Content::text("just this"))]);
    assert_eq!(content.to_text(), "just this");
    assert_eq!(content.byte_len(), "just this".len());
}

#[test]
fn cloning_blocks_shares_them() {
    let big = Content::blocks([Block::text(Content::text("x".repeat(1 << 20)))]);
    let copy = big.clone();

    let (Content::Blocks(a), Content::Blocks(b)) = (&big, &copy) else {
        unreachable!()
    };
    assert!(
        Arc::ptr_eq(a, b),
        "a turn is copied into every request that follows it; copying a megabyte each time \
         would make pruning cost more the more there was to prune"
    );
}

#[test]
fn truncating_blocks_says_how_much_of_the_whole_turn_went() {
    let whole = Content::blocks(interleaved()).byte_len();
    // what it says is a small part of what it costs, which is the case this has to get right: a
    // turn whose words fit but whose calls do not is over the limit
    assert!(whole > Content::blocks(interleaved()).to_text().len());

    // no room for the note, so the budget is spent on content
    let mut content = Content::blocks(interleaved());
    let dropped = content.truncate_to(20).expect("a turn is over 20 bytes");
    // it is text now, because a cut string is not an ordered sequence and pretending otherwise
    // would hide the truncation
    assert!(content.as_blocks().is_none());
    assert_eq!(content.to_text(), "Checking Warsaw.\nAnd");
    assert_eq!(
        dropped,
        whole - 20,
        "everything that went, not just the words"
    );

    // and with room for it, the note is the difference between the two
    let mut content = Content::blocks(interleaved());
    let dropped = content.truncate_to(80).unwrap();
    assert!(content.byte_len() <= 80);
    assert!(content.to_text().contains("truncated by an output limit"));
    let kept = content.to_text().find("\n[...").expect("a note was added");
    assert_eq!(dropped, whole - kept);
}

#[test]
fn a_turn_that_fits_is_left_alone() {
    let mut content = Content::blocks([Block::text(Content::text("short"))]);
    assert_eq!(content.truncate_to(64), None);
    assert!(content.as_blocks().is_some());
}

#[test]
fn blocks_survive_a_serde_round_trip() {
    let content = Content::blocks(interleaved());
    let json = serde_json::to_string(&content).unwrap();
    let back: Content = serde_json::from_str(&json).unwrap();

    assert_eq!(back, content);
    // the signed shape too: a thinking block a provider hands back verbatim is JSON
    let signed = Content::blocks([Block::reasoning(Content::json(
        json!({ "thinking": "...", "signature": "abc" }),
    ))]);
    let back: Content = serde_json::from_str(&serde_json::to_string(&signed).unwrap()).unwrap();
    assert_eq!(back, signed);
}

// -------------------------------------------------------------------------------- the record

#[test]
fn a_response_of_blocks_derives_its_stop_reason_and_finds_its_calls() {
    let response = ModelResponse::blocks(interleaved());

    assert_eq!(response.stop, nachalnik::StopReason::ToolUse);
    let ids: Vec<_> = response.calls().map(|call| call.id.0.as_str()).collect();
    assert_eq!(ids, ["c1", "c2"]);
    // the other way of recording the same turn is empty, because a response carrying both would
    // be two accounts of it
    assert!(response.tool_calls.is_empty());
    assert!(response.reasoning.is_none());

    let quiet = ModelResponse::blocks([Block::text(Content::text("nothing to do"))]);
    assert_eq!(quiet.stop, nachalnik::StopReason::EndTurn);
    assert_eq!(quiet.calls().count(), 0);
}

#[tokio::test]
async fn the_kernel_records_an_ordered_turn_and_runs_what_it_asked_for() {
    let kernel = kernel_with(interleaved());

    kernel.turn().await.expect("the turn ran");

    let turn = turn(&kernel);
    // the order is in the item, where a projector can reach it
    let blocks = turn.content.as_blocks().expect("recorded as blocks");
    assert_eq!(
        blocks.iter().map(Block::name).collect::<Vec<_>>(),
        ["reasoning", "text", "call", "text", "call"]
    );
    // and the kind carries no second copy of the calls
    let ContextKind::AssistantMessage {
        tool_calls,
        reasoning,
    } = &turn.kind
    else {
        unreachable!()
    };
    assert!(tool_calls.is_empty());
    assert!(reasoning.is_none());

    // the calls are still found, gated, run and answered
    assert_eq!(turn.calls().count(), 2);
    let results: Vec<_> = kernel
        .items()
        .into_iter()
        .filter(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .collect();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.content.to_text() == "sunny"));
}

#[tokio::test]
async fn an_ordered_turn_costs_what_its_calls_cost() {
    let kernel = kernel_with(interleaved());
    kernel.set_counter(Arc::new(BytesPerToken::default()));
    kernel.turn().await.expect("the turn ran");

    let turn = turn(&kernel);
    let counter = BytesPerToken::default();

    // the whole turn, not just the words: a turn whose text is short still costs whatever the
    // model wrote into the arguments
    assert_eq!(turn.tokens, counter.count_item(&turn));
    assert!(turn.tokens > counter.count(&Content::text(turn.content.to_text())));
}

#[tokio::test]
async fn a_repaired_identifier_is_repaired_where_the_call_actually_lives() {
    let kernel = kernel_with(vec![
        Block::text(Content::text("both at once")),
        Block::Call(call("", "echo", json!({}))),
        Block::Call(call("dup", "echo", json!({}))),
        Block::Call(call("dup", "echo", json!({}))),
    ]);
    let mut events = kernel.subscribe();

    kernel.turn().await.expect("the turn ran");

    let repaired = common::drain(&mut events)
        .into_iter()
        .filter(|event| matches!(event, Event::ToolCallRepaired { .. }))
        .count();
    assert_eq!(repaired, 2, "the empty one and the second `dup`");

    // and the repair landed in the blocks, so the results can be matched to the calls
    let turn = turn(&kernel);
    let ids: Vec<_> = turn.calls().map(|call| call.id.0.clone()).collect();
    assert_eq!(ids.len(), 3);
    assert!(!ids.iter().any(|id| id.is_empty()));
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "{ids:?}");

    // every result answers a call that is really there
    for item in kernel.items() {
        if let ContextKind::ToolResult { call, .. } = &item.kind {
            assert!(turn.calls().any(|asked| &asked.id == call), "{call}");
        }
    }
}

// ---------------------------------------------------------------------------- the projection

/// Projects one context with the given projector.
fn project(kernel: &Kernel, projector: LinearProjector) -> nachalnik::Projection {
    projector.project(&kernel.items())
}

#[tokio::test]
async fn flattening_produces_the_conventional_message_and_says_what_it_cost() {
    let kernel = kernel_with(interleaved());
    kernel.turn().await.expect("the turn ran");

    let projection = project(&kernel, LinearProjector::default());
    let assistant = projection
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("the turn is in the request");

    // the three slots a conventional dialect has, filled the way a provider has always assumed
    assert_eq!(
        assistant.content.as_ref().unwrap().to_text(),
        "Checking Warsaw.\nAnd now Krakow."
    );
    assert_eq!(
        assistant.reasoning.as_ref().unwrap().to_text(),
        "the user wants both cities"
    );
    assert_eq!(assistant.tool_calls.len(), 2);
    assert_eq!(assistant.calls().count(), 2);

    // and the loss is reported rather than made quietly: the second sentence came *after* a call
    // and arrives before it
    assert!(
        projection
            .repairs
            .iter()
            .any(|repair| repair.contains("flattened item") && repair.contains("ordered block")),
        "{:?}",
        projection.repairs
    );
}

#[tokio::test]
async fn a_conventional_turn_flattens_to_exactly_what_it_was() {
    let (kernel, _) = permissive([
        ModelResponse {
            content: Some(Content::text("I will look")),
            reasoning: Some(Content::text("a tool is needed")),
            tool_calls: vec![call("c1", "echo", json!({}))],
            stop: nachalnik::StopReason::ToolUse,
            usage: None,
            raw: None,
        },
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("echo", "ok")));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn ran");

    let projection = project(&kernel, LinearProjector::default());
    let assistant = projection
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .unwrap();

    assert_eq!(assistant.content.as_ref().unwrap().to_text(), "I will look");
    assert_eq!(
        assistant.reasoning.as_ref().unwrap().to_text(),
        "a tool is needed"
    );
    assert_eq!(assistant.tool_calls.len(), 1);
    // nothing was rearranged, so nothing is claimed to have been
    assert!(projection.repairs.is_empty(), "{:?}", projection.repairs);
}

#[tokio::test]
async fn sending_blocks_carries_the_order_through() {
    let kernel = kernel_with(interleaved());
    kernel.turn().await.expect("the turn ran");

    let projection = project(
        &kernel,
        LinearProjector {
            send_blocks: true,
            ..Default::default()
        },
    );
    let assistant = projection
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .unwrap();

    let blocks = assistant.blocks().expect("projected as blocks");
    assert_eq!(
        blocks.iter().map(Block::name).collect::<Vec<_>>(),
        ["reasoning", "text", "call", "text", "call"]
    );
    // the field is empty and the accessor still finds them, which is what keeps a provider
    // reading `calls()` from sending a turn with none of its calls in it
    assert!(assistant.tool_calls.is_empty());
    assert_eq!(assistant.calls().count(), 2);
    // and nothing was lost, so nothing is reported
    assert!(projection.repairs.is_empty(), "{:?}", projection.repairs);
}

#[tokio::test]
async fn sending_blocks_assembles_a_conventional_turn_into_the_conventional_order() {
    let (kernel, _) = permissive([ModelResponse::text("plain old text")]);
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn ran");

    let projection = project(
        &kernel,
        LinearProjector {
            send_blocks: true,
            ..Default::default()
        },
    );
    let assistant = projection
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .unwrap();

    // a context holding some of each projects to one shape rather than two
    let blocks = assistant.blocks().expect("assembled into blocks");
    assert_eq!(blocks.iter().map(Block::name).collect::<Vec<_>>(), ["text"]);
    assert_eq!(
        blocks[0].said().unwrap().content.to_text(),
        "plain old text"
    );
}

#[tokio::test]
async fn pruning_a_result_takes_its_call_out_of_the_ordered_turn() {
    let kernel = kernel_with(interleaved());
    kernel.turn().await.expect("the turn ran");

    // the first result goes; the call it answers has to go with it, or the request is malformed
    let first = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .unwrap();
    kernel.set_state(
        [first.id],
        ContextState::Excluded,
        Some("done with it".into()),
    );

    for send_blocks in [false, true] {
        let projection = project(
            &kernel,
            LinearProjector {
                send_blocks,
                ..Default::default()
            },
        );
        let assistant = projection
            .messages
            .iter()
            .find(|message| message.role == Role::Assistant)
            .unwrap();

        let left: Vec<_> = assistant.calls().map(|call| call.id.0.as_str()).collect();
        assert_eq!(left, ["c2"], "send_blocks: {send_blocks}");
        assert!(
            projection
                .repairs
                .iter()
                .any(|repair| repair.contains("dropped the call `c1`")),
            "send_blocks: {send_blocks}, {:?}",
            projection.repairs
        );
        // what it *said* is untouched: only the call went
        assert!(
            assistant
                .content
                .as_ref()
                .unwrap()
                .to_text()
                .contains("Checking Warsaw"),
            "send_blocks: {send_blocks}"
        );
    }
}

#[tokio::test]
async fn eliding_an_ordered_turn_keeps_the_calls_and_marks_the_words() {
    let kernel = kernel_with(interleaved());
    kernel.turn().await.expect("the turn ran");
    let turn = turn(&kernel);
    kernel.set_state(
        [turn.id],
        ContextState::Elided,
        Some("said too much".into()),
    );

    let projection = project(
        &kernel,
        LinearProjector {
            send_blocks: true,
            ..Default::default()
        },
    );
    let assistant = projection
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .unwrap();

    // the shape of the turn survives - both calls still answer their results - and only the
    // words are gone, replaced by the note whoever elided it wrote
    assert_eq!(assistant.calls().count(), 2);
    let said: Vec<_> = assistant
        .blocks()
        .unwrap()
        .iter()
        .filter_map(Block::said)
        .collect();
    assert_eq!(said.len(), 1);
    assert!(
        said[0].content.to_text().contains("said too much"),
        "{said:?}"
    );
    assert!(
        !assistant
            .content
            .as_ref()
            .unwrap()
            .to_text()
            .contains("Warsaw")
    );
    // nothing was excluded, so nothing was skipped
    assert!(projection.skipped.is_empty(), "{:?}", projection.skipped);
}

#[tokio::test]
async fn a_turn_of_nothing_but_thinking_is_left_out_and_says_so() {
    let (kernel, _) = permissive([ModelResponse::blocks([Block::reasoning(Content::text(
        "still thinking",
    ))])]);
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn ran");

    let projection = project(&kernel, LinearProjector::default());
    assert!(
        projection
            .skipped
            .iter()
            .any(|skipped| skipped.reason.contains("no content and no answered calls")),
        "{:?}",
        projection.skipped
    );
    assert!(
        !projection
            .messages
            .iter()
            .any(|m| m.role == Role::Assistant)
    );
}

#[tokio::test]
async fn a_session_of_ordered_turns_resumes_as_one() {
    let kernel = kernel_with(interleaved());
    kernel.turn().await.expect("the turn ran");

    let snapshot: Snapshot =
        serde_json::from_str(&serde_json::to_string(&kernel.snapshot()).unwrap()).unwrap();
    let resumed = Kernel::resume(Config::default(), snapshot);

    let turn = resumed
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
        .expect("the turn came back");
    assert_eq!(
        turn.content
            .as_blocks()
            .unwrap()
            .iter()
            .map(Block::name)
            .collect::<Vec<_>>(),
        ["reasoning", "text", "call", "text", "call"]
    );
    // and the identifiers it used are still spoken for, so a later turn cannot reuse one
    assert!(
        resumed
            .snapshot()
            .used_calls
            .contains(&ToolCallId::from("c1"))
    );
}

// --------------------------------------------------------------- what the two shapes must share

/// The shape of a request is not the projector's only job: a tool result has to reach the wire
/// immediately after the call it answers, and an item that lands mid-turn has to wait its turn.
/// That held for the conventional shape and not for this one, because the branch that sends
/// ordered blocks used to push its message and skip the bookkeeping underneath.
#[test]
fn an_ordered_turn_holds_back_what_landed_in_the_middle_of_it() {
    let items: Vec<Arc<ContextItem>> = [
        ContextItem::user("what is the weather?"),
        ContextItem::assistant(
            Content::blocks([
                Block::text("Checking Warsaw."),
                Block::Call(call("c1", "echo", json!({ "city": "Warsaw" }))),
            ]),
            Vec::new(),
        ),
        // what a tool wrote on the model's behalf, pushed while the turn was still collecting
        ContextItem::memory("q1", "Warsaw is in Poland"),
        ContextItem::tool_result(ToolCallId::from("c1"), "echo", "sunny", false),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, mut item)| {
        item.id = nachalnik::ContextId(index as u64 + 1);
        Arc::new(item)
    })
    .collect();

    for send_blocks in [false, true] {
        let projection = LinearProjector {
            send_blocks,
            ..Default::default()
        }
        .project(&items);

        let roles: Vec<&str> = projection
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect();
        assert_eq!(
            roles,
            ["user", "assistant", "tool", "user"],
            "send_blocks: {send_blocks} - every OpenAI-compatible API refuses a request in which \
             something else sits between a call and its result"
        );
        assert_eq!(projection.included.len(), 4, "and nothing was dropped");
        assert!(
            projection
                .repairs
                .iter()
                .any(|said| said.contains("held item")),
            "send_blocks: {send_blocks} - moving somebody's item is on the record: {:?}",
            projection.repairs
        );
    }
}

/// Eliding a turn takes its thinking with its words - whatever `send_reasoning` says. Keeping it
/// would leave a one-line marker still costing every token the model had thought, so eliding a
/// turn would free nothing at all; and here it would be worse than that, since a signed thinking
/// block would go out beside a marker that is not the words it was signed over.
#[tokio::test]
async fn eliding_an_ordered_turn_takes_its_thinking_with_the_words() {
    let kernel = kernel_with(interleaved());
    kernel.turn().await.expect("the turn ran");
    let turn = turn(&kernel);

    let projector = LinearProjector {
        send_blocks: true,
        ..Default::default()
    };
    let before = project(&kernel, projector);
    assert!(
        thinking(&before).any(|said| said.contains("the user wants both cities")),
        "the turn thought something to begin with"
    );

    kernel.set_state(
        [turn.id],
        ContextState::Elided,
        Some("said too much".into()),
    );
    let after = project(&kernel, projector);
    assert_eq!(
        thinking(&after).count(),
        0,
        "an elided turn is not one whose reasoning the model still reads"
    );
    // the shape survives, which is the whole point of eliding rather than excluding
    let assistant = after
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .unwrap();
    assert_eq!(assistant.calls().count(), 2);

    // and the flattened shape drops it too, where it would otherwise land in `Message::reasoning`
    let flat = project(&kernel, LinearProjector::default());
    assert!(
        flat.messages
            .iter()
            .all(|message| message.reasoning.is_none()),
        "{:?}",
        flat.messages
    );
}

/// What every assistant message in a projection thought, wherever it is recorded.
fn thinking(projection: &nachalnik::Projection) -> impl Iterator<Item = String> + '_ {
    projection
        .messages
        .iter()
        .flat_map(|message| {
            let ordered = message
                .blocks()
                .into_iter()
                .flatten()
                .filter_map(Block::thought)
                .map(|part| part.content.to_text().into_owned())
                .collect::<Vec<_>>();
            let flat = message
                .reasoning
                .as_ref()
                .map(|content| content.to_text().into_owned());

            ordered.into_iter().chain(flat)
        })
        .collect::<Vec<_>>()
        .into_iter()
}
