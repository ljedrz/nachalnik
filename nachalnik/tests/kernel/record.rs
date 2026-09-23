//! The paper trail: every state change as an event, the log they add up to, and what a session
//! carries across a resume.
//!
//! note: the log names things rather than copying them - `model.requested` records context ids,
//! not messages - and the exceptions are here, because each is deliberate. A payload is the
//! provider's own account of what it is about to send and is kept only when asked for; a
//! replaced item's old text is the one thing nothing else can recover; and an item's metadata,
//! first and replaced, is the hint a compactor decides by.

use std::sync::Arc;

use nachalnik::{
    Config, ContextItem, ContextKind, ContextState, Event, Kernel, ModelInfo, ModelResponse,
    Params, Record,
    test::{ConstTool, EchoTool, LargestFirstCompactor, ScriptedProvider, call},
};
use serde_json::json;

use crate::{
    common::{drain, names, permissive},
    tool_results,
};

#[tokio::test]
async fn every_step_of_the_way_is_an_event() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "echo", json!({}))]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));

    let mut events = kernel.subscribe();
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    assert_eq!(
        names(&mut events),
        [
            "context.added",   // the user's message
            "state.changed",   // idle -> requesting
            "model.requested", // ... and what it turned into
            "context.added",   // the model's turn
            "model.finished",
            "tool.requested",
            "permission.decided",
            "state.changed", // requesting -> ready
            "state.changed", // ready -> executing
            "tool.started",
            "context.added", // the tool's result
            "tool.finished",
            "state.changed", // executing -> idle
            "state.changed", // idle -> requesting
            "model.requested",
            "model.delta", // the scripted provider streams its text
            "context.added",
            "model.finished",
            "state.changed", // requesting -> finished
        ]
    );
}

#[tokio::test]
async fn the_session_log_is_append_only_and_exportable() {
    let (kernel, _) = permissive([ModelResponse::text("hello")]);
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();
    kernel.finish();

    let history = kernel.history();
    assert_eq!(history[0].event.name(), "session.started");
    assert_eq!(history.last().unwrap().event.name(), "session.finished");
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));
    assert!(
        !history.iter().any(|r| r.event.name() == "model.delta"),
        "deltas are broadcast, not recorded"
    );

    let seq = history[2].seq;
    assert_eq!(kernel.history_since(seq).len(), history.len() - 3);

    // a session is a list of events, so exporting it is a line per record
    let jsonl: Vec<String> = history
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    assert!(jsonl[0].contains(r#""event":"session.started""#));
    let restored: Record = serde_json::from_str(&jsonl[0]).unwrap();
    assert_eq!(restored, history[0]);
}

#[tokio::test]
async fn progress_can_be_recorded_too() {
    let kernel = Kernel::new(Config {
        record_progress: true,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();

    assert!(
        kernel
            .history()
            .iter()
            .any(|r| r.event.name() == "model.delta")
    );
}

#[tokio::test]
async fn a_whole_session_survives_a_round_trip() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![
            call("c1", "chatty", json!({ "value": "x" })),
            call("c2", "nope", json!({})),
        ]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(
        ConstTool::new("chatty", "x".repeat(400)).with_output_limit(100),
    ));
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor::default())));
    kernel.push(ContextItem::instruction("AGENTS.md", "no unsafe").pinned());
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    let results: Vec<_> = tool_results(&kernel).iter().map(|i| i.id).collect();
    kernel.set_state(results, ContextState::Excluded, Some("noise".into()));
    kernel.undo().unwrap();
    let mut params = Params::new();
    params.insert("temperature".into(), json!(0.0));
    kernel.set_params(params);
    kernel.finish();

    let history = kernel.history();
    assert!(history.len() > 15, "{} records", history.len());
    for record in &history {
        let json = serde_json::to_string(record).unwrap();
        let restored: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(&restored, record, "{json}");
    }
}

#[tokio::test]
async fn the_log_says_why_an_item_was_not_sent() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("first"),
        ModelResponse::text("second"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    // the user prunes the result, so the projector has to drop the call that asked for it
    let result = tool_results(&kernel)[0].id;
    kernel.set_state([result], ContextState::Excluded, Some("noise".into()));
    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.unwrap();

    let requested = kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            Event::ModelRequested {
                skipped, repairs, ..
            } => Some((skipped, repairs)),
            _ => None,
        })
        .next_back()
        .unwrap();

    // "why was that not in the request?" has to be answerable from the record alone, long after
    // the projection that decided it has been dropped
    let (skipped, repairs) = requested;
    assert!(
        skipped
            .iter()
            .any(|s| s.id == result && s.reason.contains("noise")),
        "{skipped:?}"
    );
    assert_eq!(repairs.len(), 1, "{repairs:?}");
    assert!(repairs[0].contains("dropped the call `c1`"), "{repairs:?}");

    // and it survives being written out and read back, which is the point of a log
    let json = serde_json::to_string(&kernel.history()).unwrap();
    assert!(json.contains("dropped the call `c1`"));
    let restored: Vec<Record> = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, kernel.history());
}

/// A provider that renders a payload, and sends exactly what it rendered.
struct Rendering;

#[nachalnik::async_trait]
impl nachalnik::Provider for Rendering {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("example", "renders")
    }

    fn render(&self, request: &nachalnik::ModelRequest) -> Option<serde_json::Value> {
        Some(json!({ "messages": request.messages.len(), "params": request.params }))
    }

    async fn respond(
        &self,
        _request: nachalnik::ModelRequest,
        _deltas: nachalnik::DeltaSink,
    ) -> Result<ModelResponse, nachalnik::BoxError> {
        Ok(ModelResponse::text("ok"))
    }
}

#[tokio::test]
async fn the_payload_is_the_providers_own_account_and_is_kept_only_if_asked() {
    let kernel = Kernel::new(Config {
        record_payloads: true,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(Rendering));
    kernel.push(ContextItem::user("hi"));

    // before it is sent, on demand, whatever the config says
    let previewed = kernel.preview_payload().unwrap().unwrap();
    assert_eq!(previewed["messages"], 1);

    kernel.turn().await.unwrap();
    let recorded = kernel
        .history()
        .into_iter()
        .find_map(|record| match record.event {
            Event::ModelPayload { payload } => Some(payload),
            _ => None,
        })
        .expect("the payload was asked for, so it is on the record");
    assert_eq!(
        recorded, previewed,
        "the preview is the thing that was sent"
    );

    // a provider that cannot render one says so rather than inventing it
    let (bare, _) = permissive([ModelResponse::text("ok")]);
    bare.push(ContextItem::user("hi"));
    assert_eq!(bare.preview_payload().unwrap(), None);
}

#[tokio::test]
async fn the_log_does_not_carry_payloads_unless_it_is_told_to() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(Rendering));
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();

    // the log is affordable to keep forever because its events name things rather than carrying
    // them, and a request body in every record would end that
    assert!(
        !kernel
            .history()
            .iter()
            .any(|r| r.event.name() == "model.payload")
    );
    // ... and it is still there for the asking
    assert!(kernel.preview_payload().unwrap().is_some());
}

#[tokio::test]
async fn the_log_holds_the_same_bytes_as_the_context_not_a_copy() {
    let args = json!({ "path": "src/a.rs", "text": "x".repeat(4096) });
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![nachalnik::ToolCall::new("c1", "write", args.clone())]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("write", "ok")));
    kernel.push(ContextItem::user("write it"));

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();

    let logged = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            Event::ToolRequested { args, .. } => Some(args),
            _ => None,
        })
        .unwrap();

    let turn = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    let ContextKind::AssistantMessage { tool_calls, .. } = &turn.kind else {
        unreachable!()
    };

    assert!(
        Arc::ptr_eq(&logged, &tool_calls[0].args),
        "the log should not be a second place the same bytes live"
    );

    // and a request carries the same allocation onward rather than copying it again
    kernel.push(ContextItem::user("and again"));
    let request = kernel.preview_request().unwrap();
    let sent = request
        .messages
        .iter()
        .find_map(|m| m.tool_calls.first())
        .unwrap();
    assert!(Arc::ptr_eq(&sent.args, &tool_calls[0].args));
}

/// An item's metadata is in the record from the start, and an annotation says what it replaced.
///
/// note: `context.added` named the item and left its metadata out, and `context.annotated` said
/// only what the metadata became - so what an item was added with, and every hint an annotation
/// overwrote, survived only in a snapshot taken before the change. Those are what a compactor
/// decides by, which is the reason the kernel records them at all.
#[test]
fn an_items_metadata_is_in_the_record_from_the_start() {
    let kernel = Kernel::new(Config::default());
    let id = kernel.push(ContextItem::user("hello").with_meta(json!({ "expendable": false })));
    kernel.annotate(id, json!({ "expendable": true })).unwrap();

    let history = kernel.history();
    let added = history
        .iter()
        .find_map(|record| match &record.event {
            Event::ContextAdded { meta, .. } => Some(meta.clone()),
            _ => None,
        })
        .expect("the item was added");
    assert_eq!(added, json!({ "expendable": false }));
    let (now, was) = history
        .iter()
        .find_map(|record| match &record.event {
            Event::ContextAnnotated { meta, was, .. } => Some((meta.clone(), was.clone())),
            _ => None,
        })
        .expect("the item was annotated");
    assert_eq!(now, json!({ "expendable": true }));
    assert_eq!(was, json!({ "expendable": false }));

    // and a log written before either was carried still reads
    let older: Record = serde_json::from_value(json!({
        "seq": 1, "at": 0, "event": { "event": "context.annotated", "id": 1, "meta": {} }
    }))
    .unwrap_or_else(|e| panic!("an older record reads: {e}"));
    assert!(matches!(
        older.event,
        Event::ContextAnnotated {
            was: serde_json::Value::Null,
            ..
        }
    ));
}

/// A `model.changed` written before a model said what parameters it takes still reads.
#[test]
fn a_model_change_recorded_before_parameters_were_carried_still_reads() {
    let older: Record = serde_json::from_value(json!({
        "seq": 1, "at": 0, "event": {
            "event": "model.changed",
            "from": null,
            "to": {
                "provider": "openrouter", "model": "m", "context_limit": null,
                "max_output_tokens": null, "tool_calling": true, "reasoning": false
            }
        }
    }))
    .unwrap_or_else(|e| panic!("an older record reads: {e}"));

    assert!(matches!(
        older.event,
        Event::ModelChanged { to: Some(info), .. } if info.parameters.is_empty()
    ));
}
