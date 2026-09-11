//! A settings file standing in for the command line, and the command line winning anyway.
//!
//! note: these run the program, because the whole of what is under test is the argument handling -
//! which values were typed, which were read out of a file, and which of the two a field ended up
//! with. A test that called `Settings::read` would check serde's work and none of that.
//!
//! note: what they read the answer off is the session's own output, with no model anywhere:
//! `/model` says what it would talk to, `/tools` carries the paths the sandbox opened up in
//! `shell`'s description, and `/spend` says the ceiling. Three settings of three different shapes,
//! each visible without a request being made.

use std::{io::Write, process::Command};

mod common;

/// The binary under test.
fn program() -> std::path::PathBuf {
    // the test binary lives beside it
    let mut path = std::env::current_exe().expect("a test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }

    path.join("kamchatka")
}

/// Runs it with these arguments and these lines typed at it, and hands back what a person read.
fn run(args: &[&str], lines: &str) -> (bool, String) {
    run_with(args, lines, &[])
}

/// The same, with these environment variables set over the top.
fn run_with(args: &[&str], lines: &str, env: &[(&str, &str)]) -> (bool, String) {
    let mut child = Command::new(program())
        .args(["--no-record"])
        .args(args)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        // or the model is whatever somebody running the suite has in their environment, and the
        // settings file under test would be overridden by it
        .env_remove("KAMCHATKA_MODEL")
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    child
        .stdin
        .take()
        .expect("stdin is a pipe")
        .write_all(lines.as_bytes())
        .expect("the lines were not sent");
    let out = child.wait_with_output().expect("the program never ended");

    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Writes a settings file and hands back the path.
fn settings(name: &str, json: &str) -> String {
    let path = common::scratch(name).join("kamchatka.json");
    std::fs::write(&path, json).expect("a settings file");

    path.display().to_string()
}

/// Everything in the file is what the session ends up with.
#[test]
fn a_settings_file_stands_in_for_the_command_line() {
    let path = settings(
        "settings",
        r#"{
            "model": "a-model-from-a-file",
            "sandbox-allow": ["/usr/share", "/usr/include"],
            "sandbox-read": ["~"],
            "spend": 12345
        }"#,
    );

    let (ok, said) = run(&["--config-file", &path], "/model\n/tools\n/spend\n");

    assert!(ok, "{said}");
    assert!(said.contains("a-model-from-a-file"), "{said}");
    assert!(
        said.contains("of 12,345"),
        "the ceiling did not arrive: {said}"
    );
    // the sandbox paths reach the `shell` tool's own description, which is the far end of a
    // setting that passes through `Setup` and a Landlock probe on the way
    if said.contains("read-write") {
        assert!(said.contains("/usr/share read-write"), "{said}");
        assert!(said.contains("/usr/include read-write"), "{said}");
        // and the `~` arrived as a home directory rather than as a directory called `~`. Nothing
        // is in front of a file to expand one, which is the whole reason this crate expands it
        // there and nowhere else
        let home = std::env::var("HOME").expect("a home directory");
        assert!(said.contains(&format!("{home} read-only")), "{said}");
    } else {
        eprintln!("skipped the sandbox half: nothing here confines anything");
    }
}

/// And anything typed beats it, including a value that happens to be the default.
///
/// note: the second half is the reason this does not compare against the defaults. `--requests 8`
/// is the default number and somebody who wrote it meant it; a merge that could only see the value
/// would let the file win over an argument that was actually given.
#[test]
fn the_command_line_wins() {
    let path = settings(
        "overridden",
        r#"{ "model": "the-file-model", "requests": 3, "spend": 999 }"#,
    );

    let (ok, said) = run(
        &[
            "--config-file",
            &path,
            "-m",
            "the-typed-model",
            "--spend",
            "4000",
        ],
        "/model\n/spend\n",
    );

    assert!(ok, "{said}");
    assert!(said.contains("the-typed-model"), "{said}");
    assert!(!said.contains("the-file-model"), "{said}");
    assert!(said.contains("of 4,000"), "{said}");
}

/// The environment beats the file, and the command line beats the environment.
///
/// note: `--model` is the one argument with a variable behind it, so it is the only place the
/// three can be ordered against each other - and the order is worth stating because the merge
/// could as easily have put the file above the environment: "not typed" and "not set" are
/// different questions, and this is the field where they come apart.
#[test]
fn the_environment_sits_between_them() {
    let path = settings("environment", r#"{ "model": "the-file-model" }"#);

    let (ok, said) = run_with(
        &["--config-file", &path],
        "/model\n",
        &[("KAMCHATKA_MODEL", "the-environment-model")],
    );
    assert!(ok, "{said}");
    assert!(said.contains("the-environment-model"), "{said}");

    let (ok, said) = run_with(
        &["--config-file", &path, "-m", "the-typed-model"],
        "/model\n",
        &[("KAMCHATKA_MODEL", "the-environment-model")],
    );
    assert!(ok, "{said}");
    assert!(said.contains("the-typed-model"), "{said}");
}

/// A word the file gets wrong is refused where a word on the command line would be.
///
/// note: `on-ask` is the one setting that is neither a number, a string nor a list: it is a choice
/// of two, and the file has to be held to the same two. It goes through clap's own `ValueEnum`
/// rather than a second parser here, so what a bad value is answered with is the same sentence
/// either way round.
#[test]
fn a_word_the_file_gets_wrong_is_refused() {
    let path = settings("on-ask", r#"{ "on-ask": "allow" }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(ok, "{said}");
    assert!(
        said.contains("answered `allow`"),
        "the opening line still says what the default was: {said}"
    );

    let path = settings("on-ask-wrong", r#"{ "on-ask": "maybe" }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(!ok, "a setting nobody can honour is not a success");
    assert!(said.contains("maybe"), "{said}");
    assert!(said.contains("on-ask"), "{said}");
}

/// A key nothing reads is an error, rather than a setting that quietly does nothing.
///
/// note: the failure this format is most likely to have, and the one a person cannot see: a file
/// that is accepted and ignored looks exactly like a file that worked. serde names the field and
/// lists the ones it knows, which is better than anything worth writing by hand.
#[test]
fn an_unknown_key_is_refused_by_name() {
    let path = settings("unknown", r#"{ "modle": "typo" }"#);

    let (ok, said) = run(&["--config-file", &path], "");

    assert!(!ok, "a settings file nobody can honour is not a success");
    assert!(said.contains("unknown field `modle`"), "{said}");
    assert!(
        said.contains("kamchatka.json"),
        "the file is not named: {said}"
    );
}

/// A file that is not there says so, naming it.
#[test]
fn a_missing_file_says_which() {
    let (ok, said) = run(&["--config-file", "/nowhere/kamchatka.json"], "");

    assert!(!ok);
    assert!(said.contains("/nowhere/kamchatka.json"), "{said}");
}
