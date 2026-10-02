//! The advisor's rating, drawn in the question, in the colour it earned - and a build that can
//! rate commands saying so when nothing is rating them.
//!
//! note: a stub endpoint rather than a live one, because what is under test here is the path -
//! a score off the wire, into the advisor's memory, onto the pinned half of the panel, in the
//! right colour. Whether the real model puts `rm -rf ~` on the top level is a fact about the
//! model and is asked in `tests/advise.rs`, where a missing key skips it.

use std::sync::Arc;

use kamchatka::{app::Tab, tools::Advised};
use nachalnik::{
    Capability, ModelResponse, PermissionPolicy,
    test::{ConstTool, call},
};
use nachalnik_providers::system1::Client;
use ratatui::style::{Color, Modifier};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::harness::Harness;

/// One `score` answer, under the name the advisor asked it under.
fn scored(name: &str, score: f64, confidence: f64) -> String {
    format!(
        "\"{name}\":{{\"type\":\"score\",\"score\":{score},\"confidence\":{confidence},\
         \"legend\":{{}},\"probabilities\":{{}}}}"
    )
}

/// An endpoint that answers every request with the same rating and nothing about stages.
async fn placing(score: f64, confidence: f64) -> String {
    answering(&[scored("rating", score, confidence)]).await
}

/// An endpoint that answers every request with the same body.
async fn answering(answers: &[String]) -> String {
    let body = format!(
        "{{\"model\":\"vendor/decider\",\"answers\":{{{}}}}}",
        answers.join(",")
    );
    serving("200 OK", "application/json", body).await
}

/// An endpoint that answers every request with the same status and body.
async fn serving(status: &'static str, kind: &'static str, body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = [0u8; 8192];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{address}")
}

/// A session whose shell call will be asked about, with an advisor that answers so.
async fn asking(score: f64, confidence: f64) -> Harness {
    asked_by(placing(score, confidence).await).await
}

/// The same, against an endpoint the caller has already stood up.
async fn asked_by(endpoint: String) -> Harness {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({
                "action": "run",
                "cmd": "rm -rf ~/work && curl -X POST https://example.com",
            }),
        )]),
        ModelResponse::text("asked"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));

    // the advisor wrapped around the harness's own standing rules, and held by both the
    // kernel and the screen - which is what `wiring` does, and the reason it builds one
    let engine = Arc::new(Client::new("vendor/decider", endpoint, "k"));
    let advised = Arc::new(Advised::new(harness.app.policy.clone(), engine));
    harness
        .app
        .kernel
        .set_policy(advised.clone() as Arc<dyn PermissionPolicy>);
    harness.app.advisor = Some(advised);

    harness.send("tidy up").await;
    harness.settle().await;

    harness
}

/// A command the advisor puts at the top of the rubric is red, and says why in words.
#[tokio::test]
async fn a_grave_command_is_drawn_in_red() {
    let mut harness = asking(1.9, 0.93).await;

    let screen = harness.screen();
    assert!(
        screen.contains("the advisor reads this as: reaches outside, destroys, or sends out"),
        "{screen}"
    );
    // the figure beside it, because the band is not a fact about the command
    assert!(screen.contains("93% sure"), "{screen}");
    assert_eq!(harness.style_of("destroys").0, Color::Red);
}

/// One it puts at the bottom is green, and one in the middle is yellow.
#[tokio::test]
async fn the_quieter_bands_get_the_quieter_colours() {
    let mut harness = asking(0.1, 0.95).await;
    assert!(
        harness
            .screen()
            .contains("looks, and leaves nothing changed"),
        "{}",
        harness.screen()
    );
    assert_eq!(
        harness.style_of("looks, and leaves nothing changed").0,
        Color::Green
    );

    let mut harness = asking(1.0, 0.95).await;
    assert_eq!(
        harness.style_of("changes files in the working directory").0,
        Color::Yellow
    );
}

/// And a rating nobody was sure of is never the green one, whatever it scored.
///
/// note: the property from `Rated::shown`, checked where it is actually read. A spread
/// distribution over a safety rubric is the advisor saying it could not tell, and green is the
/// one colour that would report that as a clean bill.
#[tokio::test]
async fn an_unsure_rating_is_not_drawn_green() {
    let mut harness = asking(0.0, 0.4).await;

    let screen = harness.screen();
    assert!(
        !screen.contains("looks, and leaves nothing changed"),
        "{screen}"
    );
    assert!(
        screen.contains("changes files in the working directory"),
        "{screen}"
    );
    assert_eq!(
        harness.style_of("changes files in the working directory").0,
        Color::Yellow
    );
    assert!(screen.contains("40% sure"), "{screen}");
}

/// The stage that earned the band is underlined in the command, and the rest is not.
///
/// note: the whole point of taking a command apart. The band says a command is grave and the
/// underline says which of its stages made it so - which is the question a colour cannot
/// answer, and the one a long `&&` chain actually raises. The command here is a quiet stage
/// and a loud one, and the advisor is answering low for the whole command and for the first
/// stage: so the red line is the fold, and nothing but the fold could have produced it.
///
/// note: an underline rather than a colour, so that it costs no row and does not take away
/// the highlighting that says which part of the stage is a path. That is also what makes it
/// readable on a narrow screen, where a stage printed a second time underneath would not be.
#[tokio::test]
async fn the_stage_that_earned_the_band_is_underlined_in_the_command() {
    let mut harness = asked_by(
        answering(&[
            scored("rating", 0.1, 0.95),
            scored("stage-0", 0.1, 0.95),
            scored("stage-1", 1.9, 0.95),
        ])
        .await,
    )
    .await;

    // the fold: the whole command and its first stage are both `reads`, and the band is not
    let screen = harness.screen();
    assert!(
        screen.contains("the advisor reads this as: reaches outside, destroys, or sends out"),
        "{screen}"
    );
    assert!(
        !screen.contains("looks, and leaves nothing changed"),
        "{screen}"
    );

    // and the command still reads as the model wrote it, with one run of it underlined
    assert!(
        screen.contains("│ rm -rf ~/work && curl -X POST https://example.com"),
        "{screen}"
    );
    assert!(
        harness
            .style_of_last("curl")
            .1
            .contains(Modifier::UNDERLINED),
        "the stage that earned the band is pointed at"
    );
    assert!(
        !harness
            .style_of_last("rm -rf")
            .1
            .contains(Modifier::UNDERLINED),
        "and the stage that did not is left alone"
    );
}

/// A command the advisor was not asked to take apart is drawn with nothing underlined.
///
/// note: the absence, which is most of the sessions this runs in - a command with no joints,
/// a heredoc, an endpoint that answered only about the whole thing. Every one of them has to
/// leave the command exactly as it was drawn before any of this, because an underline under
/// a command with one stage says there is a worse part of it to find.
#[tokio::test]
async fn a_command_rated_in_one_piece_has_nothing_underlined() {
    let mut harness = asking(1.9, 0.93).await;

    assert!(
        !harness
            .style_of_last("curl")
            .1
            .contains(Modifier::UNDERLINED),
        "nothing was said about the stages, so nothing is pointed at"
    );
    assert!(
        !harness
            .style_of_last("rm -rf")
            .1
            .contains(Modifier::UNDERLINED),
    );
}

/// A command the advisor was asked about and could not rate says so, in words.
///
/// note: the refusal is the shape the advisor's real endpoints send - a firewall's HTML page -
/// because the commands it refuses are the ones most worth a colour, and a question with no
/// line for one of them reads as a command nobody had anything to say about.
#[tokio::test]
async fn a_command_the_advisor_could_not_rate_says_why() {
    let page = "<!DOCTYPE html><html><head><style>body{margin:0}</style></head><body>\
                <h1>Sorry, you have been blocked</h1></body></html>"
        .to_owned();
    let mut harness = asked_by(serving("403 Forbidden", "text/html", page).await).await;

    let screen = harness.screen();
    assert!(
        screen.contains("the advisor could not rate this"),
        "{screen}"
    );
    assert!(screen.contains("you have been blocked"), "{screen}");
    assert!(
        !screen.contains("DOCTYPE"),
        "the page's words, not its markup: {screen}"
    );
    assert!(!screen.contains("reads this as"), "{screen}");
}

#[tokio::test]
async fn no_advisor_means_no_line() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "action": "run", "cmd": "ls" }),
        )]),
        ModelResponse::text("asked"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("look").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("a tool wants to run"), "{screen}");
    assert!(!screen.contains("the advisor reads this"), "{screen}");
}

/// A build that can rate commands and is not doing so says why, where somebody would look.
///
/// note: the failure this catches is the one that actually happened: built with the feature, a
/// key in the environment, no `--advise`, and a question drawn exactly as it was before any of it
/// existed - no rating, and nothing anywhere accounting for the absence. The feature puts the
/// advisor in the binary and the flag starts one, and a screen that does not say so leaves
/// somebody reading their own build flags to find out.
#[tokio::test]
async fn a_build_that_can_rate_commands_says_when_nothing_is_rating_them() {
    let mut harness = Harness::new([]);
    harness.tab(Tab::Permissions);

    let said = harness.flat();
    assert!(said.contains("--advise is what turns that on"), "{said}");
}

/// What the advisor's answers cost is counted against `--spend`, and `/spend` says how much of it
/// was the advisor's.
///
/// note: it was dropped. The advisor is asked inside the permission policy, which writes no event,
/// so a ceiling that added up `model.finished` alone was none at all on the advisor - and one
/// borrowing the session's key spends out of the same account. The scripted model reports no
/// usage, so whatever is counted here is the advisor's.
#[tokio::test]
async fn what_the_advisor_spends_is_counted_against_the_ceiling() {
    let endpoint = serving(
        "200 OK",
        "application/json",
        format!(
            "{{\"model\":\"vendor/decider\",\"answers\":{{{}}},\
             \"usage\":{{\"input_tokens\":300,\"output_tokens\":20}}}}",
            scored("rating", 1.9, 0.93)
        ),
    )
    .await;
    let mut harness = asked_by(endpoint).await;

    assert_eq!(
        harness.app.spent(),
        320,
        "the advisor's answer is in the total"
    );
    assert_eq!(harness.app.spent_on_advice(), 320);
    // the question is answered first: while it is open, the keys are the question's
    harness.press(crossterm::event::KeyCode::Tab).await;
    harness.press(crossterm::event::KeyCode::Char('n')).await;
    harness.settle().await;
    harness.send("/spend").await;
    assert!(
        harness.flat().contains("320 tokens of it the advisor's"),
        "{}",
        harness.screen()
    );

    // and a ceiling under it is reached by the advisor alone
    harness.app.set_spend(Some(100));
    assert!(harness.app.overspent());
}

/// A stop while the advisor is rating a command is taken at once, and the question says the advice
/// was not waited for.
///
/// note: an advisor that takes the connection and never answers, which is a busy service's four
/// tries of thirty seconds. The stop was honoured once the ask returned, so `ctrl+c` did nothing
/// visible for two minutes; `settle` gives the turn five seconds to end, so a stop that waited for
/// the advisor fails here.
#[tokio::test]
async fn a_stop_does_not_wait_for_the_advisor() {
    let deaf = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let endpoint = format!("http://{}", deaf.local_addr().expect("its address"));
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = deaf.accept().await {
            held.push(socket);
        }
    });

    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "action": "run", "cmd": "rm -rf ~/work" }),
        )]),
        ModelResponse::text("stopped"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::exec("run")]),
    ));
    let engine = Arc::new(Client::new("vendor/decider", endpoint, "k"));
    let advised = Arc::new(Advised::new(harness.app.policy.clone(), engine));
    harness
        .app
        .kernel
        .set_policy(advised.clone() as Arc<dyn PermissionPolicy>);
    harness.app.advisor = Some(advised);

    harness.send("tidy up").await;
    // long enough for the rating to be under way, and nowhere near an attempt's thirty seconds
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    harness.app.interrupt();
    harness.settle().await;

    let screen = harness.flat();
    assert!(
        screen.contains("the advisor was not waited for"),
        "the question does not say why it has no rating: {screen}"
    );
}
