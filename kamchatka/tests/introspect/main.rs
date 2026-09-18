//! Tests for the tools an agent inspects and manages its own context with.
//!
//! note: these drive the real loop rather than calling `Tool::invoke` by hand, because most of
//! what is worth checking is about the loop: that the tool is reached through the permission
//! policy, that what it says lands in the context as a tool result, that a fork's request really
//! is a second request to the same provider, and that changing the context from inside a turn
//! changes the request the turn goes on to send.
//!
//! note: the model is a `ScriptedProvider`, so a fork takes the next response off the same script.
//! That is not a limitation being worked around - it is what lets a test say exactly what the
//! fork was sent and exactly what it heard back.
//!
//! note: `tests/introspect/main.rs` rather than `tests/introspect.rs`, for the reason given in
//! `tests/screen/main.rs`: a directory with a `main.rs` in it is one test binary named for the
//! directory, where a crate root's submodules would each be one of their own. One file per tool,
//! the way `src/introspect/` is, with the changing half of `context` in a file of its own the way
//! it is there too - and `shared.rs` for what holds of all four whichever one is asked.

// the path is the price of the directory: `common` is shared with every other suite in here and
// stays where all of them can reach it
#[path = "../common/mod.rs"]
mod common;

mod changes;
mod context;
mod fork;
mod log;
mod search;
mod setup;
mod shared;

use std::sync::Arc;

use kamchatka::{
    introspect,
    tools::{Careful, Limits, Subject},
};
use nachalnik::{
    Config, ContextKind, Kernel, ModelResponse, Verdict,
    test::{ScriptedProvider, call},
};
use serde_json::json;

/// The branches of a tool's schema: one per shape a call may take.
///
/// note: a tool whose operations read different arguments declares a branch of an `anyOf` per
/// shape; one whose operations all read the same arguments - or which has only one - carries that
/// shape directly, because a union with one arm gates nothing and is charged for on every request.
/// Both are handled here so a test can ask about an operation without knowing which its tool is.
fn branches(schema: &serde_json::Value) -> Vec<&serde_json::Value> {
    let inside = &schema["properties"]["call"];
    match inside["anyOf"].as_array() {
        Some(several) => several.iter().collect(),
        None => vec![inside],
    }
}

/// The branch a given operation is declared in.
fn branch<'a>(schema: &'a serde_json::Value, action: &str) -> &'a serde_json::Value {
    branches(schema)
        .into_iter()
        .find(|it| {
            it["properties"]["action"]["enum"]
                .as_array()
                .is_some_and(|actions| actions.iter().any(|it| it == action))
        })
        .unwrap_or_else(|| panic!("no branch for `{action}`: {schema}"))
}

/// Every operation a tool's schema offers, in the order it offers them.
fn offers(schema: &serde_json::Value) -> Vec<&str> {
    branches(schema)
        .into_iter()
        .flat_map(|it| {
            it["properties"]["action"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("a branch names the actions it covers: {schema}"))
                .iter()
                .map(|it| it.as_str().expect("an action is a word"))
        })
        .collect()
}

/// A kernel with the tools installed, the provider that will answer it, and the handle the tools
/// reach it through - which the caller has to hold on to, or they stop working.
///
/// note: the policy this program ships, with the four capabilities answered, rather than
/// `AllowAll`. It is one line longer and it is the configuration the program is actually in - and
/// `setup permissions` reports the policy's own table, so a suite that handed it a policy the
/// kernel was not consulting would be checking a sentence about the wrong thing.
fn agent(
    script: impl IntoIterator<Item = ModelResponse>,
) -> (Kernel, Arc<ScriptedProvider>, Arc<Kernel>) {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(ScriptedProvider::new(script));
    kernel.set_provider(provider.clone());
    let policy = Arc::new(Careful::new());
    // note: the domains rather than the operations in them, which is what a rule about a whole
    // object is for: `context` covers reading this session's items and changing them alike. The
    // gating itself is `tests/policy.rs`'s to check, and it does.
    for domain in ["context", "log", "setup", "fork"] {
        policy.set(&Subject::parse(domain), Verdict::Allow);
    }
    kernel.set_policy(policy.clone());
    let anchor = introspect::install(&kernel, policy, Limits::default());

    (kernel, provider, anchor)
}

/// A tool call by id, for building an assistant turn by hand.
fn call_of(id: &str) -> nachalnik::ToolCall {
    call(id, "shell", json!({}))
}

/// A turn in which the model makes exactly these calls, and then says it is done.
fn one_turn(calls: Vec<nachalnik::ToolCall>) -> Vec<ModelResponse> {
    vec![
        ModelResponse::tool_calls(calls),
        ModelResponse::text("done"),
    ]
}

/// What the last tool result in the context says.
fn answered(kernel: &Kernel) -> String {
    kernel
        .items()
        .iter()
        .rev()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .expect("the turn recorded no tool result")
}

/// The figures on a line, in the order they are written, read back off `~1,204` and the like.
fn tokens_in(line: &str) -> Vec<usize> {
    line.split('~')
        .skip(1)
        .filter_map(|rest| {
            let figure: String = rest
                .chars()
                .take_while(|it| it.is_ascii_digit() || *it == ',')
                .filter(|it| *it != ',')
                .collect();
            figure.parse().ok()
        })
        .collect()
}

/// Every `context` result, oldest first; `answers_from` takes any other tool.
fn all_answers(kernel: &Kernel) -> Vec<String> {
    answers_from(kernel, &["context"])
}

/// Every result one of these tools produced, oldest first.
fn answers_from(kernel: &Kernel, tools: &[&str]) -> Vec<String> {
    kernel
        .items()
        .iter()
        .filter(|item| {
            matches!(&item.kind, ContextKind::ToolResult { tool, .. } if tools.contains(&tool.as_str()))
        })
        .map(|item| item.content.to_text().into_owned())
        .collect()
}
