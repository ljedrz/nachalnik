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

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use kamchatka::config::Settings;
use serde_json::json;

mod common;

/// The binary under test.
///
/// note: what cargo sets for exactly this; see `common::program`, which this cannot be, because
/// that one is `cfg(unix)` and this suite runs anywhere.
fn program() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_kamchatka"))
}

/// A directory with no settings file in it, which is where every run below is started from.
///
/// note: this suite's own working directory is the crate root, and the crate root is where the
/// shipped `kamchatka.json` lives - so once the program learned to read `./kamchatka.json`
/// without being told, every test in here was quietly running under it. What is under test is
/// which file wins, and a test standing somewhere with a file underfoot has a second answer
/// nobody wrote.
///
/// note: made once rather than per call, because `common::scratch` empties what it hands back and
/// these tests run in parallel: a shared directory wiped on the way into each of them is a race
/// between one test's file and another's.
fn elsewhere() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();

    DIR.get_or_init(|| common::scratch("cwd")).as_path()
}

/// Runs it with these arguments and these lines typed at it, and hands back what a person read.
fn run(args: &[&str], lines: &str) -> (bool, String) {
    run_with(args, lines, &[])
}

/// The same, standing in a directory of the test's choosing - which is what decides whether a
/// settings file is found underfoot.
fn run_from(dir: &Path, args: &[&str], lines: &str) -> (bool, String) {
    spawn(dir, args, lines, &[], true)
}

/// The same with no API key anywhere, which is what somebody trying this for the first time has.
///
/// note: removed rather than set empty, because an empty variable is a variable: `endpoint::connect`
/// reads one and only fails where there is none, which is the case this is about.
fn run_keyless(args: &[&str], lines: &str) -> (bool, String) {
    spawn(elsewhere(), args, lines, &[], false)
}

/// The same, with these environment variables set over the top.
fn run_with(args: &[&str], lines: &str, env: &[(&str, &str)]) -> (bool, String) {
    spawn(elsewhere(), args, lines, env, true)
}

/// One run of the program: where it stands, what it was given, and what a person read.
///
/// note: one function rather than three that had drifted. The cases differ in a directory, a key
/// and an environment, and every other line of them was the same three pipes and the same wait -
/// which is how the two of them came to disagree about which streams a caller gets back.
fn spawn(
    dir: &Path,
    args: &[&str],
    lines: &str,
    env: &[(&str, &str)],
    keyed: bool,
) -> (bool, String) {
    let mut command = Command::new(program());
    command
        .current_dir(dir)
        .args(["--no-record"])
        .args(args)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        // or the model is whatever somebody running the suite has in their environment, and the
        // settings file under test would be overridden by it
        .env_remove("KAMCHATKA_MODEL")
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    match keyed {
        true => command.env("KAMCHATKA_API_KEY", "not-a-key"),
        false => command
            .env_remove("KAMCHATKA_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("OPENAI_API_KEY"),
    };

    let mut child = command.spawn().expect("the binary under test is built");
    child
        .stdin
        .take()
        .expect("stdin is a pipe")
        .write_all(lines.as_bytes())
        .expect("the lines were not sent");
    let out = child.wait_with_output().expect("the program never ended");

    // note: both streams, because a keyless run fails before the session starts and says so on
    // stdout. Everything else a caller looks for is on stderr, which is where a headless run puts
    // what a person reads
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
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
/// A colour nobody can parse stops the program rather than being ignored.
///
/// note: this is the whole reason the value is read in `Args::under` and not where the frame is
/// drawn. A settings file is written once and then trusted, so the failure mode worth designing
/// against is not a crash - it is `"border": "#7aa2f"` sitting in a file for a month while the
/// frame stays yellow and nobody can see why the setting does nothing.
///
/// note: a headless run, which is what the suite can drive - and it is the harder case rather
/// than a dodge. A build with no screen still refuses the colour, so one settings file is either
/// valid everywhere or invalid everywhere, instead of a file that works until somebody opens it
/// on a terminal.
#[test]
fn a_border_that_is_not_a_colour_is_refused_by_name() {
    let path = settings("border-bad", r##"{ "border": "#7aa2f" }"##);

    let (ok, said) = run(&["--config-file", &path], "");

    assert!(!ok, "a colour nobody can read is not a success");
    assert!(said.contains("`border` in the settings file"), "{said}");
    assert!(said.contains("six hex digits"), "the form is shown: {said}");

    // and one that is a colour is simply taken
    let good = settings("border-good", r##"{ "border": "#7aa2f7" }"##);
    let (ok, said) = run(&["--config-file", &good], "/quit\n");
    assert!(ok, "{said}");
}

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

/// The file says which tools a session starts with, and one word moves either of them.
///
/// note: this is what `--introspect` was, and the reason it is a list in a file rather than a
/// flag. Which tools a project wants its agent to have is settled once; what a *session* wants is
/// answered at the prompt, and it used to be answerable in one direction only - `/tools drop`
/// stopped offering a tool and nothing put one back.
///
/// note: what it reads the answer off is the marks in `/tools`, which is also the only place a
/// person can see that a tool is turned off at all. A list of what is on would say nothing about
/// what to type to get the rest back.
#[test]
fn the_file_says_which_tools_a_session_starts_with() {
    let path = settings("tools", r#"{ "tools": ["fs", "context"] }"#);

    let (ok, said) = run(
        &["--config-file", &path],
        "/tools\n/tools toggle fork\n/tools\n",
    );

    assert!(ok, "{said}");
    // the two that were asked for are offered and listed as such
    assert!(said.contains("▸ fs"), "{said}");
    assert!(said.contains("▸ context"), "{said}");
    // and the ones that were not are still there to be had, rather than gone
    let off = said
        .find("· fork")
        .unwrap_or_else(|| panic!("no `fork` row: {said}"));
    assert!(said.contains("· shell"), "{said}");
    let on = said
        .find("▸ fork")
        .unwrap_or_else(|| panic!("`/tools toggle fork` offered nothing: {said}"));
    assert!(off < on, "it was offered before it was turned on: {said}");
}

/// A tool the file names that this program does not have stops it, the way an unknown key does.
///
/// note: and before the endpoint is reached, which is the second half. The answer is in the
/// arguments, and a session that connects first reports a missing API key to somebody whose
/// actual problem is a typo in their settings file - with the key set, it is a round trip spent
/// to be told something that was true before it was made.
#[test]
fn a_tool_the_file_names_that_does_not_exist_is_refused() {
    let path = settings("tools-bad", r#"{ "tools": ["fs", "contxt"] }"#);

    let (ok, said) = run(&["--config-file", &path], "");

    assert!(!ok, "a tool nobody can offer is not a success");
    assert!(
        said.contains("`contxt` is not one of this program's tools"),
        "{said}"
    );
    assert!(said.contains("context"), "and it says which are: {said}");

    // with no key anywhere, which is what somebody trying the program for the first time has
    let (ok, said) = run_keyless(&["--config-file", &path], "");
    assert!(!ok, "{said}");
    assert!(
        said.contains("`contxt` is not one of this program's tools"),
        "the settings file is answered before the endpoint is reached: {said}"
    );
    assert!(
        !said.contains("KAMCHATKA_API_KEY"),
        "and answered instead of the key, which is not what is wrong here: {said}"
    );
}

/// A path rule nothing can match stops the program, whichever door it came in by.
///
/// note: `src/**` reads like the glob `fs` takes and is not one: a path rule is a file name or one
/// directory, so that pattern was compared with file names and never matched. It was still drawn
/// on the permissions tab, and a `--deny` that refuses nothing is worse than no rule at all.
#[test]
fn a_path_rule_that_cannot_match_is_refused_at_the_door() {
    let (ok, said) = run(&["--deny", "src/**"], "");
    assert!(!ok, "a rule that refuses nothing is not a success: {said}");
    assert!(said.contains("`src/**`"), "it names the rule: {said}");
    assert!(
        said.contains("`secrets/`"),
        "and says what there is: {said}"
    );

    let path = settings("rule-bad", r#"{ "allow": ["fs:read", "vendor/*.go"] }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(!ok, "the file's rule is the program's rule: {said}");
    assert!(said.contains("`vendor/*.go`"), "{said}");

    // and the ones it can match are taken from either door
    let path = settings("rule-good", r#"{ "allow": ["vendor/", "*.lock"] }"#);
    let (ok, said) = run(&["--config-file", &path, "--deny", ".env*"], "/quit\n");
    assert!(ok, "a rule it can match was refused: {said}");
}

/// The one this crate ships works, names every setting there is, and grants nothing.
///
/// note: a starting point somebody adopts wholesale must not quietly widen anything - `allow` is
/// empty, the sandbox lists are empty and `on-ask` is `deny`, so the file changes nothing about
/// what may run. It must not quietly narrow anything either, which is the half this learnt later:
/// the file carried a spend ceiling of 200,000 tokens, on the reasoning that a tightening is the
/// one thing a file adopted sight-unseen can safely offer. What that buys is a session that stops
/// for a reason nobody chose, and the file says on its face that it is the defaults. A ceiling is
/// worth having and worth deciding on; `--spend` and `/spend` are how, and the key is here at
/// `null` so that it is one edit away.
///
/// note: the completeness check is a key-set comparison against `Settings` written out, rather
/// than a list of names here that would go stale the day a field is added. A file missing the
/// setting somebody is looking for is worth less than no file, because they stop looking.
#[test]
fn the_shipped_file_is_complete_and_grants_nothing() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/kamchatka.json");
    let text = std::fs::read_to_string(path).expect("the shipped settings file");
    let shipped: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).expect("it is JSON");

    let every: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::to_value(Settings::default()).expect("it serializes"))
            .expect("it is an object");
    let mut missing: Vec<&String> = every
        .keys()
        .filter(|key| !shipped.contains_key(*key))
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "the shipped file has no key for {missing:?}"
    );
    let mut extra: Vec<&String> = shipped
        .keys()
        .filter(|key| !every.contains_key(*key))
        .collect();
    extra.sort();
    assert!(
        extra.is_empty(),
        "the shipped file names {extra:?}, which nothing reads"
    );

    // nothing is granted, opened up or turned off by adopting it
    for key in ["allow", "deny", "sandbox-allow", "sandbox-read"] {
        assert_eq!(
            shipped[key],
            json!([]),
            "`{key}` is not empty in the shipped file"
        );
    }
    assert_eq!(shipped["on-ask"], json!("deny"));
    assert_eq!(shipped["no-sandbox"], json!(false));

    // and nothing is narrowed either. `requests` and `compact` carry the program's own defaults,
    // 8 and 0.8, which is what the file is for; the two that bound a session have no default at
    // all, so a number here would be the file deciding something nobody asked it to
    for key in ["deadline", "spend"] {
        assert_eq!(
            shipped[key],
            serde_json::Value::Null,
            "`{key}` has no default and the shipped file sets one"
        );
    }
    assert_eq!(shipped["requests"], json!(8));
    assert_eq!(shipped["compact"], json!(0.8));

    // and a session starts under it, having narrowed nothing: `/spend` is the one worth asking,
    // because a ceiling is the setting a file could most plausibly be thought to be doing a
    // favour with
    let (ok, said) = run(&["--config-file", path], "/spend\n");
    assert!(ok, "{said}");
    assert!(said.contains("no ceiling"), "{said}");
}

/// `--send-oversized` reaches the kernel, from the command line and from a file.
///
/// note: the whole of what the flag is, since the behaviour either side of it belongs to the
/// runtime and is tested there. What is only true here is the wiring - a setting that reads
/// correctly, merges correctly and then arrives nowhere is the failure a settings file has, and
/// the one nothing else in this suite would notice. The limit comes from the environment because
/// no endpoint is going to be asked what it is: the address is a closed port, so a request that
/// *is* sent fails at the socket, which is how the two outcomes are told apart.
///
/// note: the only one in this suite that names a model, and it has to: a session with none sends
/// nothing at all, so both outcomes would look like the refusal.
#[test]
fn a_request_that_looks_too_long_is_sent_when_it_is_asked_to_be() {
    let over = [("KAMCHATKA_CONTEXT_LIMIT", "10")];

    let (_, refused) = run_with(&["-m", "a-model"], "hello\n", &over);
    assert!(
        refused.contains("was not sent"),
        "the default refuses it here: {refused}"
    );

    for args in [
        vec![
            "-m".to_owned(),
            "a-model".to_owned(),
            "--send-oversized".to_owned(),
        ],
        vec![
            "-m".to_owned(),
            "a-model".to_owned(),
            "--config-file".to_owned(),
            settings("sending", r#"{"send-oversized": true}"#),
        ],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (_, sent) = run_with(&args, "hello\n", &over);
        assert!(
            !sent.contains("was not sent"),
            "asked to send it and it did not: {sent}"
        );
        assert!(
            sent.contains("127.0.0.1:1"),
            "and it went as far as the socket: {sent}"
        );
    }
}

/// A build with no MCP refuses a server it cannot run, and says nothing about a key asking for
/// none.
///
/// note: only compiled where it is true, which is `--no-default-features`. The empty half is the
/// one that bit: the shipped starting point names every key, `mcp` among them, and a rule that
/// refused the key rather than the request made that file unusable in exactly the build a settings
/// file is most useful in.
#[cfg(not(feature = "mcp"))]
#[test]
fn a_server_this_build_cannot_run_is_refused() {
    let path = settings("no-mcp", r#"{ "mcp": ["files=npx -y whatever"] }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(!ok, "a setting nobody can honour is not a success");
    assert!(said.contains("no MCP support"), "{said}");

    let path = settings("no-mcp-empty", r#"{ "mcp": [] }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(ok, "an empty list asks for nothing: {said}");
}

/// A file that is not there says so, naming it.
#[test]
fn a_missing_file_says_which() {
    let (ok, said) = run(&["--config-file", "/nowhere/kamchatka.json"], "");

    assert!(!ok);
    assert!(said.contains("/nowhere/kamchatka.json"), "{said}");
}

/// A file in the working directory is read without being named, and the session says it was.
///
/// note: the pair is the point. `cargo install` copies no files, so the shipped starting point
/// reached everybody except the people who installed this the way the readme tells them to - and
/// a file that applies because of where you are standing is a file that can surprise you. The
/// first half of that is closed by looking; the second by saying out loud what was found, into
/// the conversation rather than onto a stream a screen is about to cover.
#[test]
fn a_file_underfoot_is_read_without_being_named_and_is_said() {
    let dir = common::scratch("underfoot");
    std::fs::write(
        dir.join("kamchatka.json"),
        r#"{ "model": "a-model-from-underfoot", "spend": 4321 }"#,
    )
    .expect("a settings file where the program will stand");

    let (ok, said) = run_from(&dir, &[], "/model\n/spend\n");

    assert!(ok, "{said}");
    assert!(said.contains("a-model-from-underfoot"), "{said}");
    assert!(said.contains("of 4,321"), "{said}");
    assert!(
        said.contains("settings read from kamchatka.json"),
        "a file nobody asked for has to say it was read: {said}"
    );
}

/// A named file beats the one underfoot, and saying so is not needed for a path somebody typed.
#[test]
fn a_named_file_beats_the_one_underfoot() {
    let dir = common::scratch("both");
    std::fs::write(
        dir.join("kamchatka.json"),
        r#"{ "model": "the-one-underfoot" }"#,
    )
    .expect("a settings file to be beaten");
    let named = settings("named-over-underfoot", r#"{ "model": "the-one-named" }"#);

    let (ok, said) = run_from(&dir, &["--config-file", &named], "/model\n");

    assert!(ok, "{said}");
    assert!(said.contains("the-one-named"), "{said}");
    assert!(!said.contains("the-one-underfoot"), "{said}");
    // nothing is announced: somebody who typed the path already knows which file it was
    assert!(!said.contains("settings read from"), "{said}");
}

/// Standing nowhere in particular, nothing is read and nothing is said.
#[test]
fn a_directory_with_no_file_in_it_reads_none() {
    let dir = common::scratch("bare");

    let (ok, said) = run_from(&dir, &[], "/spend\n");

    assert!(ok, "{said}");
    assert!(!said.contains("settings read from"), "{said}");
    assert!(said.contains("no ceiling"), "{said}");
}

/// `--print-config` hands over the file this crate ships, and it is a file this program accepts.
///
/// note: the round trip rather than a byte comparison alone, because what makes the flag worth
/// having is that its output is a *starting point* - something to redirect into `kamchatka.json`
/// and edit. A copy of the shipped file that this program would then refuse would be worse than
/// no flag at all.
#[test]
fn print_config_hands_over_a_file_this_program_would_accept() {
    let out = Command::new(program())
        .arg("--print-config")
        .output()
        .expect("the binary under test is built");
    assert!(out.status.success());
    let printed = String::from_utf8(out.stdout).expect("a settings file is text");

    assert_eq!(
        printed,
        std::fs::read_to_string("kamchatka.json").expect("the shipped file"),
        "what is printed is what the repository ships"
    );

    let dir = common::scratch("printed");
    let path = dir.join("kamchatka.json");
    std::fs::write(&path, &printed).expect("written back out");
    Settings::read(&path).expect("and read back in");

    let (ok, said) = run_from(&dir, &[], "/spend\n");
    assert!(ok, "{said}");
    assert!(said.contains("settings read from kamchatka.json"), "{said}");

    // and redirected where the documentation says to put it, which the shell empties before the
    // program starts - so a program that looked for a settings file first read an empty one
    let empty = common::scratch("print-into");
    std::fs::write(empty.join("kamchatka.json"), "").expect("the file the shell truncated");
    let out = Command::new(program())
        .current_dir(&empty)
        .arg("--print-config")
        .output()
        .expect("the binary under test is built");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), printed);
}

/// `compact` is a fraction, from the command line or from a file, and a percentage is refused.
#[test]
fn a_compaction_threshold_that_is_not_a_fraction_is_refused() {
    let (ok, said) = run_with(&["--compact", "80"], "", &[]);
    assert!(!ok, "{said}");
    assert!(said.contains("rather than `80`"), "{said}");

    let dir = common::scratch("compact-percent");
    std::fs::write(dir.join("kamchatka.json"), r#"{"compact": 0}"#).expect("written");
    let (ok, said) = run_from(&dir, &[], "");
    assert!(!ok, "{said}");
    assert!(said.contains("was `0`"), "{said}");
}

/// A settings file's `system` is for a session starting, not for one carrying on.
///
/// note: the resumed context already holds it, pinned, from the run that wrote the snapshot - so
/// pushing it again on every `-r` stacked a copy per resume, each beyond compaction's reach.
#[test]
fn a_settings_files_system_instruction_is_not_pushed_again_on_resume() {
    let dir = common::scratch("resumed-system");
    std::fs::write(dir.join("kamchatka.json"), r#"{"system": "BE BRIEF"}"#).expect("written");

    let (ok, said) = run_from(&dir, &[], "/save first.json\n");
    assert!(ok, "{said}");
    let first = std::fs::read_to_string(dir.join("first.json")).expect("the session was saved");
    assert_eq!(first.matches("BE BRIEF").count(), 1, "{first}");

    let (ok, said) = run_from(&dir, &["-r", "first.json"], "/save second.json\n");
    assert!(ok, "{said}");
    let second =
        std::fs::read_to_string(dir.join("second.json")).expect("the resumed session was saved");
    assert_eq!(
        second.matches("BE BRIEF").count(),
        1,
        "the instruction was pushed into the session again: {second}"
    );
}

/// `-r` refuses a snapshot the runtime would have to repair, and says what is wrong with it.
///
/// note: a snapshot is a record, and one read back from a file may have been edited or merged. The
/// runtime renumbers an identifier two items share rather than resume a context whose lookups
/// find one of the two at random - which is right for a library that cannot fail there, and wrong
/// for a session carried on from a record that no longer says what happened.
#[test]
fn a_session_the_runtime_would_have_to_repair_is_not_carried_on_from() {
    let dir = common::scratch("resumed-repaired");
    let (ok, said) = run_from(&dir, &[], "/save first.json\n");
    assert!(ok, "{said}");

    let mut snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("first.json")).expect("saved"))
            .expect("a snapshot");
    snapshot["last_seq"] = serde_json::json!(u64::MAX);
    std::fs::write(dir.join("broken.json"), snapshot.to_string()).expect("written");

    let (ok, said) = run_from(&dir, &["-r", "broken.json"], "");
    assert!(!ok, "{said}");
    assert!(said.contains("will not carry on from"), "{said}");
    assert!(said.contains("`last_seq`"), "it says which: {said}");
}
