//! The conversation: how an answer arrives, and what it looks like once it has.
//!
//! note: what a model wrote goes through a markdown renderer, a highlighter and a wrapper before
//! anybody sees it, and every one of those can lose something. These check that it does not -
//! that a stream of fragments is one paragraph, that indentation survives, that a table keeps its
//! shape at any width, and that a long answer is never shortened on the way to the screen.

use crossterm::event::KeyCode;
use kamchatka::app::{Outcome, Speaker, Tab};
use nachalnik::{
    ContextItem, ContextState, Delta, Event, ModelInfo, ModelResponse, State, StopReason, Usage,
};
use ratatui::style::{Color, Modifier};

use crate::harness::Harness;

#[tokio::test]
async fn an_answer_that_arrives_in_fragments_is_one_paragraph_on_the_screen() {
    let mut harness = Harness::new([ModelResponse::text("a whole sentence, eventually")]);

    harness.send("say something").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("> say something"), "{screen}");
    assert!(screen.contains("a whole sentence, eventually"), "{screen}");
}

#[tokio::test]
async fn an_answer_stopped_partway_is_shown_once_rather_than_twice() {
    let mut harness = Harness::new([]);

    // exactly what the loop sees when a streamed answer is stopped mid-sentence: fragments, then
    // the interrupt, then the shortened turn being recorded. The note in the middle used to make
    // the transcript look as though nothing had streamed, and the answer was printed again
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs: Vec::new(),
    });
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Text("the beginning of a sentence".to_owned()),
    });
    harness.app.on_event(Event::Interrupted);

    let item = harness.app.kernel.push(ContextItem::assistant(
        "the beginning of a sentence",
        Vec::new(),
    ));
    harness.app.on_event(Event::ModelFinished {
        stop: StopReason::Other("interrupted".to_owned()),
        usage: None,
        tool_calls: Vec::new(),
        item,
    });

    let screen = harness.screen();
    assert_eq!(
        screen.matches("the beginning of a sentence").count(),
        1,
        "{screen}"
    );
    assert!(screen.contains("stopped"), "{screen}");
}

#[tokio::test]
async fn an_answer_from_a_provider_that_does_not_stream_still_appears() {
    let mut harness = Harness::new([]);

    // the same events, minus the fragments: nothing was drawn while it was arriving, so the
    // finished turn has to be read back off the item the kernel recorded
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs: Vec::new(),
    });
    let item = harness.app.kernel.push(ContextItem::assistant(
        "all at once, at the end",
        Vec::new(),
    ));
    harness.app.on_event(Event::ModelFinished {
        stop: StopReason::EndTurn,
        usage: None,
        tool_calls: Vec::new(),
        item,
    });

    assert!(harness.screen().contains("all at once, at the end"));
}

#[tokio::test]
async fn a_models_markdown_is_shown_as_formatting_rather_than_as_punctuation() {
    // a raw literal, because what is under test is markdown and the escapes would be the first
    // thing to get it wrong
    const ANSWER: &str = r#"## What I found

The `median` function is **wrong** for even lengths:

```rust
let mid = sorted.len() / 2;
```

---

- `mean` is fine.
"#;

    let mut harness = Harness::new([ModelResponse::text(ANSWER)]);

    harness.send("look").await;
    harness.settle().await;

    let screen = harness.screen();
    // the punctuation of the format is not the message
    assert!(!screen.contains("##"), "{screen}");
    assert!(!screen.contains("**"), "{screen}");
    assert!(!screen.contains("```"), "{screen}");
    assert!(!screen.contains('`'), "{screen}");

    // ... but everything it was marking up is still there, and marked up
    assert!(screen.contains("What I found"), "{screen}");
    assert!(screen.contains("let mid = sorted.len() / 2;"), "{screen}");
    assert!(screen.contains("- mean is fine."), "{screen}");

    let (_, heading) = harness.style_of("What I found");
    assert!(heading.contains(Modifier::BOLD), "a heading should be bold");

    let (_, emphasis) = harness.style_of("wrong");
    assert!(emphasis.contains(Modifier::BOLD), "**bold** should be bold");

    let (code, _) = harness.style_of("median");
    assert_eq!(code, Color::Cyan, "`code` should be told apart from prose");

    // a horizontal rule is drawn rather than spelled
    assert!(!screen.contains("---"), "{screen}");
    assert!(screen.contains("────"), "{screen}");

    // a fenced block gets a rule down its left rather than a slab of background
    let fenced = screen
        .lines()
        .find(|line| line.contains("let mid"))
        .expect("the block is on the screen");
    assert!(fenced.trim_start().starts_with('│'), "{fenced}");
}

#[tokio::test]
async fn what_a_tool_said_is_shown_as_the_tool_said_it() {
    let mut harness = Harness::new([]);

    // markdown is what the *model* writes. A tool's output is bytes, and running it through a
    // renderer would be inventing structure that the tool did not put there
    harness.app.say(
        kamchatka::app::Speaker::Result,
        "**not bold** and `not code` and # not a heading",
    );

    let screen = harness.screen();
    assert!(screen.contains("**not bold**"), "{screen}");
    assert!(screen.contains("`not code`"), "{screen}");
    assert!(screen.contains("# not a heading"), "{screen}");
}

#[tokio::test]
async fn a_fenced_block_is_coloured_by_what_the_tokens_are() {
    // note: a real string with real newlines; a `\` continuation would eat them, and the block
    // would arrive as one line of prose
    let answer = r#"how it works:

```rust
// the loop
fn step() { let x = 1; }
```
"#;
    let mut harness = Harness::new([ModelResponse::text(answer)]);
    harness.send("go on").await;
    harness.settle().await;

    let screen = harness.screen();

    // the fences themselves are punctuation and are not shown; the rule down the left is
    assert!(!screen.contains("```"), "{screen}");
    let code = screen
        .lines()
        .find(|line| line.contains("fn step()"))
        .expect("the block is on screen");
    assert!(code.contains("│ fn step()"), "{code}");

    // and the tokens are told apart: a keyword, a number and a comment are three colours
    let (keyword, _) = harness.style_of("fn step");
    let (digit, _) = harness.style_of("1;");
    let (comment, _) = harness.style_of("// the loop");
    assert_eq!(keyword, Color::Magenta);
    assert_eq!(digit, Color::Yellow);
    assert_eq!(comment, Color::Gray);
    assert_ne!(keyword, digit);
}

#[tokio::test]
async fn a_block_still_arriving_is_a_block_rather_than_prose() {
    // every code block is unterminated for as long as it is streaming in, and one read as prose
    // would jump from unstyled text to a coloured block the moment the closing fence landed
    let mut harness = Harness::new([]);
    harness
        .app
        .say(Speaker::Model, "here:\n\n```rust\nfn half(");

    let screen = harness.screen();

    let code = screen
        .lines()
        .find(|line| line.contains("fn half("))
        .expect("what there is of it is on screen");
    assert!(code.contains("│ fn half("), "{code}");
    assert!(!screen.contains("```"), "{screen}");
}

#[tokio::test]
async fn a_language_nothing_can_colour_is_still_a_block() {
    let mut harness = Harness::new([]);
    harness.app.say(
        Speaker::Model,
        "look:\n\n```brainfuck\n+[----->+++<]>+.\n```\n",
    );

    let screen = harness.screen();

    let code = screen
        .lines()
        .find(|line| line.contains("+[----->+++<]>+."))
        .expect("the block is on screen");
    assert!(
        code.contains("│ +[----->+++<]>+."),
        "it gets the rule whether or not anybody can colour it: {code}"
    );
}

#[tokio::test]
async fn what_a_person_reads_is_lighter_than_what_holds_it_together() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("src/parser.rs", "fn parse() {}"));
    harness.app.kernel.set_state(
        [nachalnik::ContextId(1)],
        ContextState::Excluded,
        Some("too big".into()),
    );
    harness.tab(Tab::Context);

    // the reason an item is not being sent is the column this tab exists for, and it is read
    let (reason, _) = harness.style_of("excluded: too big");
    assert_eq!(reason, Color::Gray, "not the terminal's bright black");

    // the same for the header above it, and for the count along the bottom
    let (header, _) = harness.style_of("sending");
    assert_eq!(header, Color::Gray);

    // the rule down the left of a code block is not read, and stays out of the way
    harness.app.say(Speaker::Model, "```rust\nfn f() {}\n```\n");
    harness.tab(Tab::Chat);
    let (bar, _) = harness.style_of("│ fn f()");
    assert_eq!(bar, Color::DarkGray, "chrome, not words");
}

#[tokio::test]
async fn a_pasted_block_arrives_as_the_lines_it_was_pasted_as() {
    let mut harness = Harness::new([]);

    // what a terminal sends: the breaks inside a paste are carriage returns, because a paste is
    // spelled as though it had been typed and that is what enter sends
    harness.app.paste("first line\rsecond line\r\nthird line");

    assert_eq!(
        harness.app.input.lines(),
        ["first line", "second line", "third line"],
        "a paste is the lines it was, not one line with invisible characters in it"
    );
    let screen = harness.screen();
    for line in ["first line", "second line", "third line"] {
        assert!(
            screen.contains(line),
            "{line} is not in the prompt: {screen}"
        );
    }
}

#[tokio::test]
async fn a_message_sent_into_a_running_turn_is_answered_when_it_ends() {
    let mut harness = Harness::new([
        ModelResponse::text("the first answer"),
        ModelResponse::text("the second answer"),
    ]);

    harness.send("the first question").await;
    // ... and this one goes in while that turn is still running. Nothing used to come back to it:
    // the turn was already going, so nothing started, and it sat in the context unanswered
    harness.app.busy = true;
    harness.send("the second question").await;
    harness.app.busy = false;
    harness.app.on_outcome(Outcome::Stopped(State::Idle));

    assert!(harness.app.busy, "a turn was started for it");
    harness.settle().await;
    let screen = harness.screen();
    assert!(screen.contains("the second answer"), "{screen}");
}

#[tokio::test]
async fn a_message_sent_into_a_running_turn_goes_in_after_the_answer_it_interrupted() {
    let mut harness = Harness::new([ModelResponse::text("the first answer")]);

    harness.send("the first question").await;
    harness.app.busy = true;
    harness.send("the second question").await;

    // ... which is not in the context yet, because the answer being written is not in it either
    let during: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(
        !during.iter().any(|text| text.contains("the second")),
        "a message pushed here lands in front of the answer the model is still writing: {during:?}"
    );

    harness.app.busy = false;
    harness.settle().await;

    // the conversation reads in the order it happened, and ends with the person - which is the
    // one shape a request is allowed to have
    let after: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    let question = after
        .iter()
        .position(|text| text.contains("the second question"))
        .expect("it went in when the turn ended");
    let answer = after
        .iter()
        .position(|text| text.contains("the first answer"))
        .expect("the answer it waited for");
    assert!(answer < question, "{after:?}");
}

#[tokio::test]
async fn a_message_that_has_to_wait_says_that_it_is_waiting() {
    let mut harness = Harness::new([ModelResponse::text("the first answer")]);

    harness.send("the first question").await;
    harness.app.busy = true;
    harness.send("the second question").await;

    // it is on the screen and not yet in the context, which is the one moment those two disagree.
    // Saying so is the difference between a message waiting and a message that went nowhere
    let screen = harness.screen();
    assert!(
        screen.contains("goes in when the turn stops"),
        "a queued message says it is queued: {screen}"
    );
}

#[tokio::test]
async fn a_conversation_stays_where_somebody_scrolled_it_while_the_model_keeps_writing() {
    let mut harness = Harness::new([]);
    let write = |harness: &mut Harness, from: usize, to: usize| {
        for n in from..to {
            harness.app.on_event(Event::ModelDelta {
                delta: Delta::Text(format!("line {n}\n\n")),
            });
        }
    };

    write(&mut harness, 0, 80);
    let screen = harness.screen();
    assert!(screen.contains("line 79"), "{screen}");

    // scroll back to read something from further up
    harness.press(KeyCode::PageUp).await;
    harness.press(KeyCode::PageUp).await;
    let read = harness.screen();
    let anchor = read
        .match_indices("line ")
        .next()
        .map(|(at, _)| {
            read[at..]
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .expect("something is on the screen");
    assert!(!read.contains("line 79"), "{read}");

    // the model writes another eighty lines under it. Every fragment used to yank the window back
    // to the newest of them, which made reading anything it had said earlier impossible until the
    // turn was over
    write(&mut harness, 80, 160);
    let after = harness.screen();
    assert!(after.contains(&anchor), "the view moved: {after}");
    assert!(!after.contains("line 159"), "{after}");
    // and the window says there is more underneath, rather than just stopping
    assert!(after.contains("ctrl+e follows"), "{after}");

    // ctrl+e is the way back down without paging through what arrived in between
    harness.chord(KeyCode::Char('e')).await;
    let back = harness.screen();
    assert!(back.contains("line 159"), "{back}");

    // and it keeps following from there
    write(&mut harness, 160, 200);
    let screen = harness.screen();
    assert!(screen.contains("line 199"), "{screen}");
}

#[tokio::test]
async fn a_message_of_your_own_takes_you_back_to_the_bottom() {
    let mut harness = Harness::new([]);
    for n in 0..80 {
        harness.app.on_event(Event::ModelDelta {
            delta: Delta::Text(format!("line {n}\n\n")),
        });
    }
    harness.screen();
    harness.press(KeyCode::PageUp).await;
    harness.press(KeyCode::PageUp).await;
    assert!(!harness.screen().contains("line 79"));

    // typing something and sending it is somebody saying they are done reading back
    harness.send("what about this").await;
    let screen = harness.screen();
    assert!(screen.contains("> what about this"), "{screen}");
}

#[tokio::test]
async fn a_table_too_wide_for_the_window_keeps_its_shape() {
    let mut harness = Harness::new([]);
    harness.app.say(
        Speaker::Model,
        "Here is a comparison:\n\n\
         | seam | you provide | the kernel provides |\n\
         | --- | --- | --- |\n\
         | `Provider` | a model | the request, verbatim |\n\
         | `Tool` | what it can do | the schema and the gating |\n\n\
         and some text after it.\n",
    );
    harness.tab(Tab::Chat);

    // wide enough: the box is drawn at the width its contents want, and the backticks are gone
    let roomy = harness.sized(90, 20);
    assert!(roomy.contains("│ seam     │ you provide    │"), "{roomy}");
    assert!(
        !roomy.contains('`'),
        "inline code is styled, not spelled: {roomy}"
    );

    // too narrow: the columns give, not the borders. Every line of the table is the same width
    // and has a border at each end - the markdown renderer's own table was wrapped like a
    // sentence, so half of a border arrived on the next line and the whole thing came apart
    let narrow = harness.sized(46, 24);
    // the window's own frame taken off, so that what is left is the table's
    let rows: Vec<String> = narrow
        .lines()
        .map(|line| {
            let mut cells: Vec<char> = line.chars().collect();
            cells.resize(46, ' ');

            cells[1..45]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    let table: Vec<&String> = rows
        .iter()
        .filter(|line| line.starts_with(['┌', '│', '├', '└']))
        .collect();
    assert!(table.len() > 6, "the table is drawn: {narrow}");
    for line in &table {
        assert!(
            line.ends_with(['┐', '│', '┤', '┘']),
            "a row that does not close: {line:?} in {narrow}"
        );
        assert_eq!(
            line.chars().count(),
            table[0].chars().count(),
            "a row of a different width: {line:?} in {narrow}"
        );
    }
    // and nothing was lost to make it fit
    assert!(narrow.contains("verbatim"), "{narrow}");
    assert!(narrow.contains("gating"), "{narrow}");
}

#[tokio::test]
async fn a_table_is_only_a_table_where_one_was_written() {
    let mut harness = Harness::new([]);
    harness.app.say(
        Speaker::Model,
        "```md\n| not | a | table |\n| --- | --- | --- |\n| it is | in | a fence |\n```\n\n\
         | left | middle | right |\n| :--- | :----: | ----: |\n| a | b |\n| c | d | e | f |\n",
    );
    harness.tab(Tab::Chat);
    let screen = harness.sized(80, 24);

    // a table inside a fence is a code block, pipes and all
    assert!(screen.contains("| not | a | table |"), "{screen}");

    // the colons say which way a column reads, and a short row is squared up rather than left
    // ragged; a long one is cut to the columns the header declared
    assert!(screen.contains("│ a    │   b    │       │"), "{screen}");
    assert!(screen.contains("│ c    │   d    │     e │"), "{screen}");
}

/// A long answer keeps its beginning.
///
/// note: a live session lost the top of one. The transcript bounded a *still arriving* entry at
/// eight thousand bytes and replaced whatever came before with `[...]`, which is right for a
/// `find /` and wrong for everything else - a model writing a long answer had its first paragraphs
/// eaten while it was still writing the last, and nothing ever put them back: the finished item is
/// read off the kernel only for a provider that did not stream.
#[tokio::test]
async fn a_long_answer_is_not_shortened_while_it_arrives() {
    let mut harness = Harness::new([]);
    for n in 0..400 {
        harness.app.on_event(Event::ModelDelta {
            delta: Delta::Text(format!("this is sentence number {n} of a long answer.\n\n")),
        });
    }

    let said = &harness
        .app
        .transcript
        .last()
        .expect("the model said something")
        .text;
    assert!(said.len() > 16_000, "the test is not testing anything");
    assert!(
        said.contains("sentence number 0 of"),
        "the beginning of the answer is gone"
    );
    assert!(said.contains("sentence number 399 of"), "and so is the end");
    assert!(!said.contains("[...]"), "nothing was cut: {}", &said[..80]);

    // ... and it is all there to be scrolled back through
    harness.screen();
    harness.chord(KeyCode::Home).await;
    let top = harness.screen();
    assert!(top.contains("sentence number 0 of"), "{top}");
}

/// The same for a person's own message, however long it is.
#[tokio::test]
async fn a_long_message_of_your_own_is_not_shortened_either() {
    let mut harness = Harness::new([]);
    let long = format!(
        "here is a question: {}",
        "and some more of it. ".repeat(600)
    );
    harness.app.say(kamchatka::app::Speaker::User, long.clone());

    let said = &harness.app.transcript.last().expect("it is there").text;
    assert_eq!(said, &long);
}

/// `ctrl+home` and `ctrl+end` are the two ends of the conversation; `home` and `end` are the
/// prompt's own, as they are everywhere else.
#[tokio::test]
async fn the_two_ends_of_the_conversation_are_one_key_each() {
    let mut harness = Harness::new([]);
    for n in 0..120 {
        harness.app.on_event(Event::ModelDelta {
            delta: Delta::Text(format!("line {n}\n\n")),
        });
    }
    let screen = harness.screen();
    assert!(screen.contains("line 119"), "{screen}");

    harness.chord(KeyCode::Home).await;
    let top = harness.screen();
    assert!(top.contains("line 0"), "{top}");
    assert!(!top.contains("line 119"), "{top}");
    // and it stays there while more arrives, like every other way of scrolling back
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Text("line 120\n\n".to_owned()),
    });
    let still = harness.screen();
    assert!(still.contains("line 0"), "{still}");

    harness.chord(KeyCode::End).await;
    let bottom = harness.screen();
    assert!(bottom.contains("line 120"), "{bottom}");
    // ... and follows from there, which is what `ctrl+e` does and what the bottom means
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Text("line 121\n\n".to_owned()),
    });
    assert!(harness.screen().contains("line 121"));

    // on their own they are the prompt's, and the conversation does not move
    harness.chord(KeyCode::Home).await;
    harness.screen();
    for c in "a question".chars() {
        harness.press(KeyCode::Char(c)).await;
    }
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Char('!')).await;
    let typed = harness.screen();
    assert!(
        typed.contains("line 0"),
        "home moved the conversation: {typed}"
    );
    assert!(
        typed.contains("!a question"),
        "home is the start of the line being typed: {typed}"
    );
    harness.press(KeyCode::End).await;
    harness.press(KeyCode::Char('?')).await;
    let typed = harness.screen();
    assert!(
        typed.contains("line 0"),
        "end moved the conversation: {typed}"
    );
    assert!(
        typed.contains("!a question?"),
        "and end is the end of it: {typed}"
    );
}

/// What the model is no longer shown comes off the conversation, and is still on the record.
///
/// note: dropped rather than marked, which is the whole decision, and it is the opposite of the
/// one this file used to make. The argument for marking was that the chat is the record of what
/// happened - but there are two views here and the context tab is already that record: an
/// excluded turn is listed on it, holding everything it held, one keystroke from coming back.
/// What nothing showed was the conversation the model is actually in, and a transcript that
/// keeps every turn anybody ever took it out of is not that conversation. So the chat answers
/// "what is being sent" and the context tab answers "what happened", and neither has to answer
/// both badly.
#[tokio::test]
async fn the_chat_drops_what_the_model_is_no_longer_shown() {
    let mut harness = Harness::new([ModelResponse::text("crabs probably do not wonder about it")]);
    harness.send("do crabs think that fish can fly?").await;
    harness.settle().await;

    let before = harness.screen();
    assert!(before.contains("crabs probably do not wonder"), "{before}");

    // taken out of the request by hand, which is `space` on the context tab or `/exclude`
    let answered = harness.app.kernel.items()[1].id;
    harness
        .app
        .kernel
        .set_state([answered], ContextState::Excluded, Some("by hand".into()));

    let after = harness.screen();
    assert!(
        !after.contains("crabs probably do not wonder"),
        "an excluded turn is in no request, so it is in no conversation: {after}"
    );
    assert!(
        after.contains("do crabs think"),
        "and the question that is still going is still here: {after}"
    );

    // nothing was destroyed to do it: the turn is still a row on the context tab, still holding
    // what it holds - the row spends its last column on why it is out rather than on the words,
    // and `enter` on it is still the way to read them
    harness.tab(Tab::Context);
    let listed = harness.screen();
    assert!(
        listed.contains("assistant_message"),
        "the record keeps the row: {listed}"
    );
    assert!(listed.contains("excluded: by hand"), "and why: {listed}");
    assert_eq!(
        harness.app.kernel.item(answered).unwrap().content.to_text(),
        "crabs probably do not wonder about it",
        "and the item still holds every byte of it, which is what `enter` on the row reads"
    );

    // and putting it back puts it back, because the chat is a reading of the context and not a
    // second account of it
    harness
        .app
        .kernel
        .set_state([answered], ContextState::Active, None);
    harness.tab(Tab::Chat);
    let restored = harness.screen();
    assert!(
        restored.contains("crabs probably do not wonder"),
        "restored to the request is restored to the conversation: {restored}"
    );
}

/// An elided turn stays, because an elided turn is still in the request.
///
/// note: the line between the two, and the reason the check is the item's state rather than
/// whether the projector emitted content for it. Excluded is gone from the request entirely and
/// goes; elided is in the request as a one-line marker, so the conversation keeps its place and
/// marks it - which is what the model is reading there too.
#[tokio::test]
async fn an_elided_turn_keeps_its_place_and_says_it_is_a_marker() {
    let mut harness = Harness::new([ModelResponse::text("a long and detailed answer")]);
    harness.send("go on").await;
    harness.settle().await;

    let answered = harness.app.kernel.items()[1].id;
    harness.app.kernel.set_state(
        [answered],
        ContextState::Elided,
        Some("compacted to make room".into()),
    );

    let screen = harness.screen();
    assert!(
        screen.contains("a long and detailed answer"),
        "it is still in the request, so it is still readable: {screen}"
    );
    assert!(
        screen.contains("compacted to make room"),
        "and says why the model is only getting a marker: {screen}"
    );
    let marked = screen
        .lines()
        .any(|row| row.contains("a long and detailed answer") && row.contains('╎'));
    assert!(
        marked,
        "drawn so the eye can tell without reading: {screen}"
    );
}

/// Both halves of a turn go, not just the one the walk reached first.
///
/// note: the regression a live run found and this file did not, and it outlived the change from
/// marking to dropping because its cause is the attribution rather than the drawing. A turn puts
/// what it thought and what it said on the screen as two entries; attributing the second stamped
/// it, and attributing the first then stopped at the stamp - so one of the two carried the
/// turn's identifier and the other carried none. Under marking that left the thinking reading as
/// live above a marked answer; under dropping it leaves the thinking on the screen with the
/// answer gone, which is worse. Same bug, same test, louder failure.
#[tokio::test]
async fn a_turn_drops_what_it_thought_as_well_as_what_it_said() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("go on then"));

    // what a streaming provider produces: thinking, then words, then the recorded item
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Reasoning("weighing it up".to_owned()),
    });
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Text("here is the answer".to_owned()),
    });
    let answered = harness
        .app
        .kernel
        .push(ContextItem::assistant("here is the answer", vec![]));
    harness.app.on_event(Event::ModelFinished {
        item: answered,
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Some(Usage::default()),
    });

    harness
        .app
        .kernel
        .set_state([answered], ContextState::Excluded, Some("by hand".into()));

    let screen = harness.screen();
    // both halves go, the thinking included: half a turn on the screen is an account of a
    // conversation nobody had
    for line in ["weighing it up", "here is the answer"] {
        assert!(
            !screen.contains(line),
            "{line:?} is not in the request, so it should not be on the chat: {screen}"
        );
    }
    // and the reason it went is on the tab that keeps the record, not on this one
    harness.tab(Tab::Context);
    let listed = harness.screen();
    assert!(listed.contains("by hand"), "{listed}");
}

/// An edit reads where the turn was, and says what it used to be.
///
/// note: the alternative - saying the new words at the end of the transcript - describes an order
/// no request ever had, because an edit to a turn from twenty exchanges ago would land after
/// everything that followed it. What the model reads is the new words in the old place, so that
/// is what the conversation shows.
#[tokio::test]
async fn an_edited_turn_reads_where_it_was_and_says_what_it_used_to_be() {
    let mut harness = Harness::new([ModelResponse::text("nay")]);
    harness.send("do crabs think that fish can fly?").await;
    harness.settle().await;

    // `e` on the answer, three keys to clear it, then the words somebody puts in its mouth
    harness.tab(Tab::Context);
    harness.press(KeyCode::End).await;
    harness.press(KeyCode::Char('e')).await;
    assert_eq!(harness.app.input.lines(), ["nay"]);
    for _ in 0..3 {
        harness.press(KeyCode::Backspace).await;
    }
    harness.send("of course they do").await;

    harness.tab(Tab::Chat);
    let screen = harness.screen();
    let row = |needle: &str| screen.lines().position(|line| line.contains(needle));

    // the edit is in the conversation, and the turn it replaced is not sitting beside it
    assert!(screen.contains("of course they do"), "{screen}");
    assert!(
        !screen.contains("nay"),
        "the superseded text should not still be on the chat: {screen}"
    );
    // in the place the old turn had, rather than after everything
    assert!(row("do crabs think") < row("of course they do"), "{screen}");
    // and the line says what it used to be, and where what it said has gone
    assert!(screen.contains("[2] → [3]"), "{screen}");
    assert!(screen.contains("edited here"), "{screen}");
    assert!(
        screen.contains("reads what it said"),
        "the row should say where the old words went: {screen}"
    );
}

/// An edit that has been undone comes off the conversation with the item it named.
///
/// note: the chat's account of an edit is a reading of the context rather than a copy written into
/// the transcript, and this is the difference between the two. `undo` takes the replacement item
/// back out and tells the screen nothing about which line had been moved onto it, so a transcript
/// holding the new words went on showing them - beside a row offering `enter on [3]` for an item
/// that no longer existed, against a context that had the original answer back. Showing somebody
/// a conversation the model is not in is the one thing this program exists not to do.
#[tokio::test]
async fn undoing_an_edit_takes_it_off_the_conversation_too() {
    let mut harness = Harness::new([ModelResponse::text("nay")]);
    harness.send("do crabs think that fish can fly?").await;
    harness.settle().await;

    harness.tab(Tab::Context);
    harness.press(KeyCode::End).await;
    harness.press(KeyCode::Char('e')).await;
    for _ in 0..3 {
        harness.press(KeyCode::Backspace).await;
    }
    harness.send("of course they do").await;

    // `u` puts the answer back, and the conversation says what the context says
    harness.press(KeyCode::Char('u')).await;
    harness.tab(Tab::Chat);
    let undone = harness.screen();
    assert!(undone.contains("nay"), "the answer is back: {undone}");
    assert!(
        !undone.contains("of course they do"),
        "the edit is out of the context, so it is off the chat: {undone}"
    );
    assert!(
        !undone.contains("edited here"),
        "and nothing points at the item the undo took away: {undone}"
    );

    // and `U` is the way back, all the way back
    harness.tab(Tab::Context);
    harness.press(KeyCode::Char('U')).await;
    harness.tab(Tab::Chat);
    let redone = harness.screen();
    assert!(redone.contains("of course they do"), "{redone}");
    assert!(redone.contains("[2] → [3]"), "{redone}");
}

/// The blank lines a provider puts in front of a turn do not reach the screen.
///
/// note: `inception/mercury` opens every message with two of them and the recorded `gemini`
/// sessions have none, so this is the provider's habit rather than anything the runtime did. The
/// item keeps what arrived - a record of "what arrived, tidied up" cannot answer what arrived -
/// and the screen declines to spend rows on it.
///
/// note: the thinking rather than the answer, because the answer is rendered as markdown and the
/// renderer swallows them already. Everything else - the thinking, a tool's output, an item's
/// pages - is shown as the text it is, and that is where the padding was being read.
#[tokio::test]
async fn a_turn_that_arrives_padded_is_not_read_padded() {
    let mut harness = Harness::new([]);
    harness.app.say(Speaker::User, "say something");
    // the shape mercury sends: two newlines, then the words
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Reasoning("\n\nweighing it up".to_owned()),
    });
    let answered = harness
        .app
        .kernel
        .push(ContextItem::assistant("no, crabs do not", vec![]));
    harness.app.on_event(Event::ModelFinished {
        item: answered,
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Some(Usage::default()),
    });

    let screen = harness.screen();
    let rows: Vec<&str> = screen.lines().collect();
    let at = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} should be on the screen: {screen}"))
    };

    // one blank row between them, which is the separator the chat puts there itself
    let gap = at("weighing it up") - at("say something");
    assert_eq!(
        gap,
        2,
        "expected one blank row between them, got {}",
        gap - 1
    );
}

/// A turn rewritten in place reads as it is now, not as it arrived.
///
/// note: the gap between the two ways content changes, and the one nothing covered. A terminal
/// edit *supersedes*: a new item, a new identifier, and the transcript line is re-pointed at it.
/// `amend revise` **replaces**, in place, so the identifier never moves - and the chat went on
/// showing the words that had streamed in while the context tab, the `enter` overlay and the
/// request itself all showed the new ones. Nothing on the screen said which of the two the model
/// had actually read.
///
/// note: so the entry's own text is what arrived and the item is what is being sent, and the
/// chat reads the item. Which also means an `undo` reaches the screen without anything here
/// keeping a second account of what to put back.
#[tokio::test]
async fn a_turn_rewritten_in_place_reads_as_it_is_now() {
    let mut harness = Harness::new([ModelResponse::text("the words that streamed in")]);
    harness.send("say something").await;
    harness.settle().await;
    assert!(harness.screen().contains("the words that streamed in"));

    // what `amend revise` does: same item, different content, no new identifier
    let answered = harness.app.kernel.items()[1].id;
    harness
        .app
        .kernel
        .replace(answered, "the words somebody put in its mouth")
        .expect("the item is there");

    let after = harness.screen();
    assert!(
        after.contains("the words somebody put in its mouth"),
        "the chat should read the item, not the fragments: {after}"
    );
    assert!(
        !after.contains("the words that streamed in"),
        "and not both at once, which would be two accounts of one turn: {after}"
    );

    // and it is not a copy written into the transcript: undo takes it back and the screen agrees
    harness.app.kernel.undo();
    let undone = harness.screen();
    assert!(
        undone.contains("the words that streamed in"),
        "undo puts the words back in the context, so it puts them back here: {undone}"
    );
    assert!(!undone.contains("put in its mouth"), "{undone}");
}

/// A message sent with `--message` is tied to its item like any other.
///
/// note: it was not, and the way that showed is the reason `App::ask` exists. Startup said the
/// line and pushed the item as two statements and never attributed them to each other, so the
/// first message of every `-m` session was a transcript line no context change could reach: it
/// could not be dropped when excluded and could not be updated when rewritten, for the whole
/// session. Three lines in a row, one of them forgotten in one of the three places.
#[tokio::test]
async fn a_message_from_the_command_line_is_tied_to_its_item() {
    let mut harness = Harness::new([]);

    let asked = harness.app.ask("what main does with -m");
    assert_eq!(
        harness.app.kernel.item(asked).unwrap().content.to_text(),
        "what main does with -m"
    );
    assert!(harness.screen().contains("what main does with -m"));

    harness
        .app
        .kernel
        .set_state([asked], ContextState::Excluded, Some("by hand".into()));
    let screen = harness.screen();
    assert!(
        !screen.contains("what main does with -m"),
        "an attributed line goes when its item does: {screen}"
    );
}
