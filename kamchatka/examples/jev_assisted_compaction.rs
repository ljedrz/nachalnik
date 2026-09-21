//! Deciding what a full context can afford to lose, with TypeSafe's `jev`.
//!
//! ```console
//! TYPESAFE_API_KEY=apikey_... \
//!   cargo run -p kamchatka --features advise --example jev_assisted_compaction
//! ```
//!
//! note: or `KAMCHATKA_API_KEY`, which asks the same model through OpenRouter. It goes through
//! [`endpoint::advise::connect`], which is what `--advise` itself goes through, so the two
//! choose the account and the model the same way - this used to ask for `TYPESAFE_API_KEY` and
//! nothing else, which left the one example about the advisor unrunnable for anybody whose key
//! is the one the feature is happiest with.
//!
//! An agent's context fills up, and something has to go. `kamchatka` ships [`Trim`], which elides
//! the oldest tool results — and age is a *proxy*. Nobody wants the oldest gone; they want the
//! least useful gone, and "useful" is a relation between a result and what the session is trying
//! to do. Nothing in a terminal client could evaluate that, so the pass answers the question it
//! can instead.
//!
//! [`Jev`] can answer the real one. It is a System One model: it takes a state and a map of typed
//! questions and returns numbers — no text, no tool calls, nothing to parse. Here the state is
//! everything the user typed over the session plus a digest of each candidate result, and there is
//! one `score` question per candidate against an ordered rubric. All of them go in **one
//! request**, evaluated in parallel and in isolation, so no candidate's answer can be moved by
//! another's and the round trip is paid once however many there are.
//!
//! The context below is synthetic and rigged in exactly one way: the result the task depends on
//! is the *oldest*. That is not a trick — it is the ordinary shape of a session, since you read
//! the file you are working on first and then spend twenty turns running things against it, and
//! it is the shape age gets wrong every time.
//!
//! What this compares is the **order**, holding the number of items taken fixed: `Trim` decides
//! how many it must elide to get under its target, and the same count is taken off the other end
//! of the ranking. So the two are choosing between the same alternatives. It deliberately does
//! not reimplement the rest of `Trim` — the marker arithmetic, the floor under what is worth
//! eliding, the superseded summary — because a copy of those in an example would be a subtly
//! wrong one.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use kamchatka::{endpoint, tools::Trim};
use nachalnik::{
    BoxError, Compactor, Config, ContextItem, ContextKind, ContextState, Kernel, ModelInfo,
    test::{ScriptedProvider, call},
};
use nachalnik_providers::{Endpoint, system1::Question};
use serde_json::json;

/// The rubric each candidate is placed on, least worth keeping first.
///
/// note: a `score` rather than a `choice`, because the answer is a *position* on an ordered list
/// and may fall between two levels — which is what a ranking wants. A `choice` over the same four
/// would have thrown the ordering away and made two adjacent answers incomparable.
///
/// note: phrased about the task rather than about the content. "Large" and "old" are properties of
/// an item; "still needed" is a relation between an item and the work, and that relation is the
/// thing age was standing in for.
///
/// note: `marked` is the level that is not a judgement about the result's own content, and it is
/// reachable only because the state carries what the *user* said. Somebody typing "this is
/// important:" or "remember that" is the whole of the signal, it lives in the transcript where
/// they typed it, and nothing on the result itself records it. That is why the state below sends
/// the user's messages in order rather than only the opening task — take them away and this level
/// becomes unreachable, since no amount of reading a spec file tells you the user asked for it.
const RUBRIC: [&str; 4] = [
    "oneshot: nothing since refers to it and the conversation has moved past it",
    "obsolete: what it established has already been said later in the conversation",
    "referenced: later work builds on something in it",
    "marked: the user said this one is important",
];

/// One thing that happened, in the order it happened.
enum Step {
    /// The user typing at the prompt.
    Said(&'static str),
    /// A tool being run, and what it answered.
    Ran {
        /// What ran, which is the item's label and much of what the ranking goes on.
        label: &'static str,
        /// The output itself.
        body: &'static str,
    },
}

/// The session, oldest first, and between them the results cover every level of [`RUBRIC`].
///
/// note: rigged in exactly one way — the result the work is actually about is the *oldest*, so
/// age gets the one decision that matters maximally wrong. That is not a trick: it is the ordinary
/// shape of a session, since you read the file you are working on first and then spend twenty
/// turns running things against it.
///
/// note: `obsolete` needs a pair to be obsolete *against*, which is why the failing `cargo build`
/// and the later passing one are both here. A level defined as "already said later in the
/// conversation" cannot be demonstrated by one item on its own.
///
/// note: the two [`Step::Said`] entries are load-bearing rather than scene-setting. The second one
/// is the only thing in the whole session that makes `read docs/chunked-framing.md` more than
/// another file somebody opened, and it is four steps away from the result it is about.
const SESSION: &[Step] = &[
    Step::Said(
        "The `parse_header` function in src/wire.rs is returning the wrong length for chunked \
         responses. Fix it.",
    ),
    // the function under repair, and the comment naming the bug: what the whole session is for
    Step::Ran {
        label: "read src/wire.rs",
        body: "pub fn parse_header(buf: &[u8]) -> Result<Header, Error> {\n    let len = \
               read_u32(&buf[4..8])?;\n    // NOTE: chunked responses put the length in the \
               trailer, not here - this branch has never handled them\n    if flags & CHUNKED != \
               0 {\n        return Ok(Header { len, chunked: true });\n    }\n    Ok(Header { \
               len, chunked: false })\n}\n",
    },
    // obsolete: it failed for a reason the later build shows fixed
    Step::Ran {
        label: "shell cargo build",
        body: "error[E0425]: cannot find value `CHUNKED` in this scope\n  --> src/wire.rs:18:17\n \
               \nerror: could not compile `wire` due to 1 previous error\n",
    },
    // oneshot: nothing refers to any of these again
    Step::Ran {
        label: "shell ls -la",
        body: "total 48\ndrwxr-xr-x  8 user user 4096 Sep 18 09:14 .\ndrwxr-xr-x 14 user user \
               4096 Sep 18 09:02 ..\n-rw-r--r--  1 user user 1044 Sep 18 09:11 Cargo.toml\n",
    },
    // the whole of the `marked` signal, typed in passing and never repeated
    Step::Said(
        "this is important: the length lives in the trailer, per docs/chunked-framing.md — \
         remember that when you write the fix",
    ),
    // marked: nothing about this file says the user cares. The sentence above does
    Step::Ran {
        label: "read docs/chunked-framing.md",
        body: "# chunked framing\n\nA chunked response carries its length in the *trailer*, after \
               the final zero-length chunk. The header's length field is reserved and MUST be \
               ignored by readers. Implementations that read it see 0 or garbage.\n",
    },
    // referenced: the call sites any fix has to keep working
    Step::Ran {
        label: "grep -rn parse_header src/",
        body: "src/wire.rs:14:pub fn parse_header(buf: &[u8]) -> Result<Header, \
               Error> {\nsrc/reader.rs:88:        let header = \
               parse_header(&self.buf)?;\nsrc/reader.rs:203:    let header = \
               parse_header(chunk)?;\nsrc/tests/framing.rs:31:    let h = \
               parse_header(FIXTURE).unwrap();\n",
    },
    // the later work that makes the grep above `referenced` rather than another spent lookup. A
    // level defined as "later work builds on something in it" needs that later work to be in the
    // session: the first draft of this example asserted the grep was referenced and nothing ever
    // used it, and the ranking said `obsolete` and was right
    Step::Ran {
        label: "fs edit src/reader.rs",
        body: "replaced 2 occurrences\n  src/reader.rs:88  parse_header(&self.buf)? -> \
               parse_header(&self.buf).and_then(|h| h.resolve_len(&self.trailer))?\n  \
               src/reader.rs:203  parse_header(chunk)? -> parse_header(chunk).and_then(|h| \
               h.resolve_len(trailer))?\n",
    },
    Step::Ran {
        label: "shell cargo tree --depth 1",
        body: "wire v0.4.0\n├── anyhow v1.0.99\n├── bytes v1.10.1\n└── thiserror v2.0.17\n",
    },
    Step::Ran {
        label: "shell git status",
        body: "On branch master\nYour branch is up to date with 'origin/master'.\n\nnothing to \
               commit, working tree clean\n",
    },
    // and the build that makes the failing one above obsolete
    Step::Ran {
        label: "shell cargo build --quiet",
        body: "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.21s\n",
    },
];

/// What the context is measured against, and it is deliberately smaller than the context.
///
/// note: the session below comes to more than this, so the next request would be refused outright
/// rather than merely be getting close. A demonstration sitting at sixty percent of its limit
/// invites the reader to ask why anything is being elided at all, and the answer - a threshold
/// somewhere below full - is not what this is about.
///
/// note: sized with `THRESHOLD` so the pass takes about half. Taking one would show nothing and
/// taking eight would leave nothing to compare against; which half goes is the whole point.
const LIMIT: usize = 6_000;

/// How full the context may get before a pass runs; `kamchatka`'s own default.
const THRESHOLD: f64 = 0.8;

/// How much of each candidate is shown to the model, in bytes.
///
/// note: a head excerpt rather than the whole thing, since the whole thing is what the context is
/// too full of — sending it to be judged would cost more than the pass recovers. Most of the
/// signal is not in the content anyway: it is in the label, which for a tool result is the command
/// or the path that produced it, and `grep -r todo src/` is distinguishable from `cat src/main.rs`
/// without reading either result.
const EXCERPT: usize = 400;

/// Pads a result to something a compaction pass would bother with.
///
/// note: the pass refuses to elide anything smaller than the marker that replaces it, so a
/// demonstration built out of four-line outputs would print two empty plans and show nothing. What
/// is padded is the bulk, never the first lines — those are what the excerpt carries.
fn padded(body: &str) -> String {
    format!("{body}{}", "\n// ... more output ...".repeat(120))
}

/// A context in the shape a session gets into: a task, and six results behind it.
fn context() -> Kernel {
    let kernel = Kernel::new(Config::default());
    // a `Budget` takes its limit from the provider's `ModelInfo`, so one has to be plugged in even
    // though nothing here ever sends a request. It is scripted and has no answers in it, which is
    // the honest shape: what is being compared happens *before* a request, and the model that
    // would answer one is not part of the story
    kernel.set_provider(Arc::new(ScriptedProvider::new([]).with_info(ModelInfo {
        provider: "none".to_owned(),
        model: "none".to_owned(),
        context_limit: Some(LIMIT),
        max_output_tokens: None,
        tool_calling: false,
        reasoning: false,
        parameters: Vec::new(),
    })));

    for (n, step) in SESSION.iter().enumerate() {
        match step {
            Step::Said(text) => kernel.push(ContextItem::user(*text)),
            Step::Ran { label, body } => {
                // the assistant turn that asked for it, and not decoration: a tool result whose
                // call is nowhere in the context is dropped by the projector, since most endpoints
                // reject a result with no call. A context of bare results projects to nothing and
                // no pass ever runs
                let call = call(&format!("call-{n}"), label, json!({}));
                kernel.push(ContextItem::assistant(String::new(), vec![call.clone()]));
                kernel.push(ContextItem::tool_result(
                    call.id.clone(),
                    *label,
                    padded(body),
                    false,
                ))
            }
        };
    }

    kernel
}

/// The first [`EXCERPT`] bytes of a result, cut on a character boundary.
fn excerpt(text: &str) -> String {
    if text.len() <= EXCERPT {
        return text.to_owned();
    }
    let mut room = EXCERPT;
    while room > 0 && !text.is_char_boundary(room) {
        room -= 1;
    }

    format!("{}… ({} bytes in all)", &text[..room], text.len())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    // the program's own path to an advisor, so that a key this feature accepts is a key this
    // example accepts. It reads `KAMCHATKA_SYSTEM1_API_KEY`, `TYPESAFE_API_KEY` and
    // `KAMCHATKA_API_KEY` in that order and picks the endpoint and the model to match
    let jev = endpoint::advise::connect(&endpoint::session_endpoint(false)).await?;
    if let Some(notice) = jev.take_notice() {
        eprintln!("advisor: {notice}");
    }

    let kernel = context();
    let items = kernel.items();
    let budget = kernel.budget();

    // the same filter `Trim` uses, and the only one of its rules this needs: a tool result that is
    // still sending its content and is not pinned
    let candidates: Vec<_> = items
        .iter()
        .filter(|item| {
            item.state.sends_content()
                && item.state != ContextState::Pinned
                && matches!(item.kind, ContextKind::ToolResult { .. })
        })
        .collect();

    // every message the user typed, in order, read back off the context rather than kept in a
    // constant. This is where `marked` comes from and it is the reason the whole transcript of
    // them goes out rather than the opening task alone
    let said: Vec<String> = items
        .iter()
        .filter(|item| item.kind == ContextKind::UserMessage)
        .map(|item| item.content.to_text().into_owned())
        .collect();

    println!(
        "the rubric — a score is a position on this scale, and may land between two levels:\n"
    );
    for (n, level) in RUBRIC.iter().enumerate() {
        let (name, means) = level.split_once(": ").unwrap_or((level, ""));
        println!("  {n}  {name:<11} {means}");
    }

    println!("\nwhat the user asked, over the session:");
    for text in &said {
        println!("  · {text}");
    }
    println!(
        "\ncontext: {} tokens against a {LIMIT}-token limit — {}% of it, so the next request \
         would be refused.",
        budget.used(),
        (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
    );

    // ---- what the real compactor does, which is the age order
    let by_age = Trim::under(THRESHOLD)
        .plan(&items, &budget)
        .await
        .ok_or("the context was not full enough to compact")?;

    // ---- and the same decision put to `jev`: one `score` per candidate, all in one request
    let state = json!({
        "the_user_said": said,
        "results": candidates
            .iter()
            .map(|item| json!({
                "id": item.id.to_string(),
                "produced_by": item.label,
                "tokens": item.tokens,
                "excerpt": excerpt(&item.content.to_text()),
            }))
            .collect::<Vec<_>>(),
    });
    let questions: Vec<_> = candidates
        .iter()
        .map(|item| {
            (
                item.id.to_string(),
                // note: "worth keeping" rather than "does the task depend on it", which is what
                // this asked first and was a question narrower than the rubric it is scored
                // against. `marked` is not about dependence at all - it is about something the
                // user said - so a question that only asked about dependence left that level
                // unreachable by construction, and the answers duly never went near it
                Question::score(
                    format!(
                        "Result {} was produced by `{}`. How much is it still worth keeping in \
                         the context?",
                        item.id, item.label
                    ),
                    RUBRIC,
                ),
            )
        })
        .collect();

    let started = std::time::Instant::now();
    let answers = jev.ask(state, questions).await?;
    let took = started.elapsed();

    let mut ranked: Vec<_> = candidates
        .iter()
        // a candidate nobody answered for sorts last: an unanswered question is not evidence that
        // an item is spent, and the failure that matters is taking something the task needed
        .map(|item| {
            (
                item,
                answers.score(&item.id.to_string()).unwrap_or(f64::MAX),
            )
        })
        .collect();
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    // what each pass would take. `Trim` decides how many have to go; the same count comes off the
    // bottom of the ranking, so the two are choosing between the same alternatives
    let going = by_age.elide.len();
    let cut_by_age: BTreeSet<_> = by_age.elide.iter().copied().collect();
    let cut_by_jev: BTreeSet<_> = ranked.iter().take(going).map(|(item, _)| item.id).collect();
    let scored: BTreeMap<_, _> = ranked
        .iter()
        .map(|(item, score)| (item.id, *score))
        .collect();

    println!(
        "\n{going} of the {} results have to go — ✂ is elided, · is kept:\n",
        candidates.len()
    );
    println!(
        "  {:>5}  {:<11} {:<30}  age  jev",
        "score", "level", "result"
    );
    // in the order the session happened, so the age pass can be seen working down from the top
    for item in &candidates {
        let score = scored.get(&item.id).copied().unwrap_or(f64::NAN);
        let level = RUBRIC[(score.round() as usize).min(RUBRIC.len() - 1)]
            .split(':')
            .next()
            .unwrap_or("?");
        let mark = |cut: bool| if cut { "✂" } else { "·" };

        println!(
            "  {score:>5.2}  {level:<11} {:<30}  {}    {}",
            item.label,
            mark(cut_by_age.contains(&item.id)),
            mark(cut_by_jev.contains(&item.id)),
        );
    }

    // the line a reader should leave with, and it is computed rather than asserted: whatever the
    // model happened to say, this names what the two passes actually disagreed about
    let saved: Vec<&str> = candidates
        .iter()
        .filter(|item| cut_by_age.contains(&item.id) && !cut_by_jev.contains(&item.id))
        .map(|item| item.label.as_str())
        .collect();
    if let Some((last, rest)) = saved.split_last() {
        let listed = match rest.is_empty() {
            true => last.to_string(),
            false => format!("{} and {last}", rest.join(", ")),
        };
        println!(
            "\nage takes {listed} — the ranking keeps {} and spends the room on results \
             nothing has referred to since.",
            match saved.len() {
                1 => "it",
                _ => "all of them",
            }
        );
    }

    println!(
        "\n{} questions in one request, answered in {:.2}s.",
        ranked.len(),
        took.as_secs_f64(),
    );
    if let Some(usage) = answers.usage {
        // whoever actually answered, rather than the service that makes the model: the same
        // `jev` is served by TypeSafe's own API and by OpenRouter, and a line naming the wrong
        // one is a line about somebody else's bill
        println!(
            "It cost {} in / {} out at {} to decide which of {} tokens of the agent's own \
             context to keep — a different endpoint, and a different bill.",
            usage.input_tokens.unwrap_or_default(),
            usage.output_tokens.unwrap_or_default(),
            jev.host(),
            budget.used(),
        );
    }

    Ok(())
}
