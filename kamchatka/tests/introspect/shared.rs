//! What is true of all four: the handle they reach the kernel through, the wrapper a call's
//! arguments arrive in, and the ceiling on what any of them hands back.

use crate::{agent, answered, answers_from, common, one_turn};
use kamchatka::{tools::Careful, tools::Limits};
use nachalnik::{ContextItem, ModelResponse, OutputSink, test::call};
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc};

#[tokio::test]
async fn the_tools_stop_working_when_the_handle_goes() {
    let (kernel, _provider, anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));

    kernel.push(ContextItem::user("look at yourself"));
    // whoever installed them has gone; the tools are still registered and still answer, and what
    // they answer is why they cannot do anything
    drop(anchor);

    kernel.turn().await.expect("the turn failed");

    assert!(answered(&kernel).contains("this session is over"));
}

/// An answer names only the tools this session actually has.
///
/// note: bought by a live run. `/tools toggle log` took the log away mid-session, and `setup tools`
/// went on ending with "`log` with `kinds: [\"tools.changed\"]` says when it went" - advice naming
/// a tool in the same breath as reporting that the model does not have it. It is the rule a
/// refusal already follows: everything named in an answer is read as something to try, so name
/// only what can be reached.
#[tokio::test]
async fn an_answer_does_not_point_at_a_tool_that_has_been_taken_away() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![
            call("c1", "setup", json!({ "action": "tools" })),
            call("c2", "setup", json!({ "action": "permissions" })),
        ]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![
            call("c3", "setup", json!({ "action": "tools" })),
            call("c4", "setup", json!({ "action": "permissions" })),
        ]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what have you got?"));
    kernel.turn().await.expect("the turn failed");

    let with_it = answers_from(&kernel, &["setup"]);
    assert!(with_it[0].contains("tools.changed"), "{}", with_it[0]);
    assert!(with_it[1].contains("permission.decided"), "{}", with_it[1]);

    // the reader of the record goes; the record does not
    kernel.remove_tool("log");
    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.expect("the second turn failed");

    let without = answers_from(&kernel, &["setup"]);
    for said in &without[2..] {
        assert!(
            !said.contains("`log`"),
            "an answer that names a tool the session does not have is advice nobody can take: \
             {said}"
        );
    }
    // and the rest of the answer is untouched: what is gone is the sentence, not the report
    assert!(
        without[2].contains("tool(s), which is every one"),
        "{}",
        without[2]
    );
    assert!(
        without[3].contains("`ask` is nobody having decided yet"),
        "{}",
        without[3]
    );
}

/// A call asks permission for the one thing it does, however its arguments are wrapped.
///
/// note: the regression this is here for. These tools take their arguments inside a `call` object,
/// and `Tool::needs` on `context` was reading the outside of it - so it found no `action`, took
/// the branch meant for a call nobody can place, and declared all twelve subjects. A `look` at
/// three items asked the person to allow `context:revise` and `context:elide` along with it, and a
/// session holding `--allow context:look` and nothing else could not look at all.
///
/// note: over every tool this program installs rather than over `context`, because the bug was not
/// `context`'s. It was that `needs` and `invoke` read the arguments two different ways, which is
/// available to any tool here and invisible until somebody reads a permission question.
#[tokio::test]
async fn a_call_needs_the_one_subject_it_names_through_the_wrapper() {
    let (kernel, _provider, _anchor) = agent(Vec::new());
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
        },
        kamchatka::sandbox::Reach {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: false,
        },
        Limits::default(),
    ) {
        kernel.add_tool(tool);
    }

    let wanted = [
        (
            "context",
            json!({ "action": "look", "ids": [14, 15, 17], "whole": true }),
            "context:look",
        ),
        (
            "context",
            json!({ "action": "note", "content": "x", "reason": "y" }),
            "context:note",
        ),
        ("fs", json!({ "action": "read", "path": "a.rs" }), "fs:read"),
        (
            "fs",
            json!({ "action": "edit", "path": "a.rs", "old": "x", "new": "y" }),
            "fs:edit",
        ),
        ("shell", json!({ "action": "run", "cmd": "ls" }), "exec:run"),
        ("log", json!({ "action": "read" }), "log:read"),
        ("setup", json!({ "action": "tools" }), "setup:tools"),
        ("fork", json!({ "action": "draft" }), "fork:draft"),
    ];

    for (id, args, subject) in wanted {
        let tool = kernel.tool(id).expect("it is installed");
        for (how, args) in [("wrapped", json!({ "call": args.clone() })), ("flat", args)] {
            let needs = tool.needs(&call("c1", id, args));
            let named: Vec<String> = needs.iter().map(ToString::to_string).collect();
            assert_eq!(
                named,
                [subject],
                "{id} asks for {} subjects on a {how} call that does one thing",
                needs.len()
            );
        }
    }
}

/// Every subject a registered tool declares has a row in the limits table, and nothing else does.
///
/// note: the vocabulary is written out a fourth time in `Limits::new`, and it is the copy nothing
/// held to the others. A tool that grew an operation would declare a subject the table had no row
/// for, and the consequence is quiet in both directions: `for_call` finds nothing and falls back
/// to the kernel's ceiling, and `/limit` cannot list a row it does not have, so the number a
/// person reads is not the number in force.
///
/// note: the table cannot be derived from the tools, because it is handed *to* them - they take it
/// at construction and read it afresh on every call, which is what lets `/limit` land on the next
/// one. So the two lists stay two, and this is what keeps them the same list.
#[tokio::test]
async fn the_limits_table_has_a_row_for_every_subject_a_tool_declares() {
    let (kernel, _provider, _anchor) = agent(Vec::new());
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
        },
        kamchatka::sandbox::Reach {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: false,
        },
        Limits::default(),
    ) {
        kernel.add_tool(tool);
    }

    let declared: BTreeSet<String> = kernel
        .tool_specs()
        .iter()
        .flat_map(|spec| spec.capabilities.iter().map(ToString::to_string))
        .collect();
    let rows: BTreeSet<String> = Limits::new()
        .all()
        .into_iter()
        .map(|(subject, _)| subject)
        .collect();

    let missing: Vec<&String> = declared.difference(&rows).collect();
    assert!(
        missing.is_empty(),
        "these are declared and have no limit row, so `/limit` cannot see them: {missing:?}"
    );
    let extra: Vec<&String> = rows.difference(&declared).collect();
    assert!(
        extra.is_empty(),
        "these have a limit row and nothing declares them, so `/limit` lists a number that is \
         never consulted: {extra:?}"
    );
}

/// The subjects on the lower limit are the ones whose answer does not grow with the session.
///
/// note: `Limits` holds two numbers - 32,000 bytes for an answer made of what the session holds,
/// 8,000 for one that is a report of a fixed shape - and which tier a subject is in is a claim
/// about how its answer is built. This is that claim, checked: the same seven calls against ten
/// items and against a thousand, with two hundred more tools registered for the two `setup` ones,
/// and none of the answers may move. A `budget` that listed every item, or a `revise` that quoted
/// what it wrote, would be a subject that had quietly changed tier, and the first anybody would
/// otherwise know of it is a cut answer in a live session.
///
/// note: the figures are the point of the pairing rather than the absolute sizes, so it asserts
/// both: no answer grows by more than the identifiers in it can (`[3]` becomes `[997]`), and every
/// one is inside the tier with room to spare.
#[tokio::test]
async fn the_answers_on_the_lower_limit_do_not_grow_with_the_session() {
    let dir = common::scratch("bounded-answers");
    std::fs::write(dir.join("w.rs"), "x".repeat(40_000)).expect("a file to act on");

    // the seven, and one from each tool that has any, so a whole tool moving tier is visible here
    let calls = |path: &std::path::Path| -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (
                "fs",
                json!({"call": {"action": "write", "path": path.join("w.rs"),
                                   "content": "y".repeat(39_700) + &"z".repeat(300)}}),
            ),
            (
                "fs",
                // note: a run of `old` that occurs once. `y` three hundred times over occurs all
                // through a file of `y`, and the edit refused every time - so what this measured
                // was the refusal
                json!({"call": {"action": "edit", "path": path.join("w.rs"),
                                   "old": "z".repeat(300), "new": "w".repeat(300)}}),
            ),
            ("context", json!({"call": {"action": "budget"}})),
            (
                "context",
                json!({"call": {"action": "note", "content": "a note",
                                        "reason": "measuring"}}),
            ),
            (
                "context",
                json!({"call": {"action": "revise", "ids": [3],
                                        "content": "q".repeat(30_000), "reason": "measuring"}}),
            ),
            ("setup", json!({"call": {"action": "model"}})),
            ("setup", json!({"call": {"action": "policy"}})),
        ]
    };

    // one session of each size, answering the same calls
    let mut sizes: Vec<Vec<usize>> = Vec::new();
    for (items, extra) in [(10, 0), (1_000, 200)] {
        let (kernel, _provider, _anchor) = agent(Vec::new());
        for tool in kamchatka::tools::builtin(
            kamchatka::tools::Shell {
                workdir: dir.clone(),
                extra: Vec::new(),
                readable: Vec::new(),
                policy: Arc::new(Careful::new()),
                confiner: None,
                limits: Limits::default(),
            },
            kamchatka::sandbox::Reach {
                workdir: dir.clone(),
                extra: Vec::new(),
                readable: Vec::new(),
                confined: false,
            },
            Limits::default(),
        ) {
            kernel.add_tool(tool);
        }
        for n in 0..items {
            let body = match n % 7 {
                0 => "x".repeat(40_000),
                _ => format!("item number {n}, saying a sentence about what it holds"),
            };
            kernel.push(ContextItem::file(format!("src/file_{n}.rs"), body));
        }
        for n in 0..extra {
            kernel.add_tool(Arc::new(
                nachalnik::test::ConstTool::new(format!("server__tool_{n}"), "did it")
                    .with_capabilities([nachalnik::Capability::fs("read")]),
            ));
        }

        let mut answers = Vec::new();
        for (n, (tool, args)) in calls(&dir).into_iter().enumerate() {
            let made = call(&format!("m{n}"), tool, args);
            let tool = kernel.tool(tool).expect("a registered tool");
            let out = nachalnik::Tool::invoke(&*tool, &made, nachalnik::OutputSink::disconnected())
                .await
                .expect("the call was answered");
            let said = out.content.to_text();
            assert!(
                !out.is_error,
                "the call has to have worked for its size to mean anything: {said}"
            );
            assert!(
                !said.contains("is required") && !said.contains("no such item"),
                "the call has to have worked for its size to mean anything: {said}"
            );
            answers.push(said.len());
        }
        sizes.push(answers);
    }

    let names = [
        "fs:write",
        "fs:edit",
        "context:budget",
        "context:note",
        "context:revise",
        "setup:model",
        "setup:policy",
    ];
    for ((name, small), big) in names.iter().zip(&sizes[0]).zip(&sizes[1]) {
        assert!(
            *big <= small + 256,
            "{name} answers {small} bytes about ten items and {big} about a thousand, so it is \
             not a report of a fixed shape and does not belong on the lower limit"
        );
        assert!(
            *big < 8_000,
            "{name} answers {big} bytes, which the limit it is on would cut"
        );
    }
}

/// `shell` says where its output is cut off, and says the number this session is holding.
///
/// note: it said "long output is cut off at the end", which a model finds the edge of by spending
/// a call on it. The figure is read off the limits table rather than written into the sentence
/// because `/limit exec:run` moves it and a description is built afresh for every request - so a
/// session that raised the limit and a description that still quoted the old one would be this
/// program telling a model something it had itself just made untrue.
#[tokio::test]
async fn the_shell_says_how_much_of_an_answer_it_will_hand_back() {
    let limits = Limits::default();
    let shell = kamchatka::tools::Shell {
        workdir: std::path::PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        policy: Arc::new(Careful::new()),
        confiner: None,
        limits: limits.clone(),
    };

    let said = nachalnik::Tool::spec(&shell).description;
    assert!(said.contains("32000 bytes is cut off"), "{said}");

    // and the sentence follows the table rather than repeating what it said at startup
    limits.set("exec:run", 4_000);
    let said = nachalnik::Tool::spec(&shell).description;
    assert!(said.contains("4000 bytes is cut off"), "{said}");
    assert!(!said.contains("32000"), "{said}");
}

/// Every operation works when its arguments arrive the way the schema asks for them.
///
/// note: the shape nothing was testing. The schema tells a model to put its arguments inside a
/// `call` object; `inner` also accepts them flat, because refusing an unambiguous call costs a
/// turn - and every test in this workspace was written before the wrapper existed, so all 133 of
/// them take the flat path and the real one was exercised by almost nothing. Three readers had
/// already been found reading the outside of the wrapper by hand.
///
/// note: `invoke` directly rather than through a turn, because what is under test is reading the
/// arguments and not the policy, the projector or the provider. An operation that failed to unwrap
/// answers "`` is not something `fs` does" or "the `path` argument is required" - both errors, so
/// the assertion is simply that nothing came back as one.
#[tokio::test]
async fn every_operation_works_with_its_arguments_inside_the_wrapper() {
    let dir = common::scratch("every-operation-wrapped");
    std::fs::write(dir.join("a.rs"), "fn main() {}\n").expect("a file to act on");

    let (kernel, _provider, _anchor) = agent(Vec::new());
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            workdir: dir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
        },
        kamchatka::sandbox::Reach {
            workdir: dir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            // confined, so a relative path resolves against the directory above rather than
            // against wherever cargo started this process
            confined: true,
        },
        Limits::default(),
    ) {
        kernel.add_tool(tool);
    }
    kernel.push(ContextItem::user("something to act on"));
    kernel.push(ContextItem::user("and a second thing"));

    // one call per operation, in an order that leaves the context usable for the next: the reads
    // first, then a note to have something of this tool's own to move, then the moves over it
    let wanted: Vec<(&str, serde_json::Value)> = vec![
        ("fs", json!({ "action": "read", "path": "a.rs" })),
        ("fs", json!({ "action": "glob", "pattern": "*.rs" })),
        ("fs", json!({ "action": "grep", "pattern": "fn" })),
        (
            "fs",
            json!({ "action": "write", "path": "b.rs", "content": "//\n" }),
        ),
        (
            "fs",
            json!({ "action": "edit", "path": "b.rs", "old": "//", "new": "// x" }),
        ),
        ("shell", json!({ "action": "run", "cmd": "echo hello" })),
        ("log", json!({ "action": "read" })),
        ("setup", json!({ "action": "model" })),
        ("setup", json!({ "action": "tools" })),
        ("setup", json!({ "action": "permissions" })),
        ("setup", json!({ "action": "policy" })),
        ("context", json!({ "action": "look" })),
        ("context", json!({ "action": "budget" })),
        (
            "context",
            json!({ "action": "search", "text": "something" }),
        ),
        (
            "context",
            json!({ "action": "note", "content": "a finding", "reason": "why" }),
        ),
        (
            "context",
            json!({ "action": "revise", "ids": [1], "content": "changed", "reason": "why" }),
        ),
        (
            "context",
            json!({ "action": "elide", "ids": [1], "reason": "why" }),
        ),
        (
            "context",
            json!({ "action": "exclude", "ids": [1], "reason": "why" }),
        ),
        (
            "context",
            json!({ "action": "pin", "ids": [2], "reason": "why" }),
        ),
        (
            "context",
            json!({ "action": "restore", "ids": [2], "reason": "why" }),
        ),
        ("context", json!({ "action": "undo", "reason": "why" })),
        ("context", json!({ "action": "redo", "reason": "why" })),
    ];

    for (id, args) in wanted {
        let action = args["action"].as_str().expect("each names one").to_owned();
        let tool = kernel.tool(id).expect("it is installed");
        let call = call("c1", id, json!({ "call": args }));
        let out = tool
            .invoke(&call, OutputSink::disconnected())
            .await
            .unwrap_or_else(|e| panic!("`{id}: {action}` did not run at all: {e}"));

        assert!(
            !out.is_error,
            "`{id}: {action}` refused a call in the shape its own schema asks for: {}",
            out.content
        );
    }
}
