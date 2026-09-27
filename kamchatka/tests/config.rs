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
    let mut command = Command::new(common::program());
    command
        .current_dir(dir)
        .args(["--no-record"])
        .args(args)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        // or the model is whatever somebody running the suite has in their environment, and the
        // settings file under test would be overridden by it
        .env_remove("KAMCHATKA_MODEL")
        // and the same for an advisor, which would otherwise be found where no test put one
        .env_remove("SYSTEM1_ADVISOR_COMMAND")
        .env_remove("KAMCHATKA_SYSTEM1_API_KEY")
        .env_remove("TYPESAFE_API_KEY")
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
    assert!(said.contains(&path), "the file is named: {said}");
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

/// A file the command line says nothing about is named in every refusal, whichever half of the
/// startup is making it.
///
/// note: the half that bites is a file nobody named. `Args::under` holds a value to the file and
/// says which key, and then `setup` and the wiring hold it to what this program can do - a
/// fraction, a tool, a server, a path rule - with nothing said about where it came from. A file
/// found underfoot is announced on standard error, where a screen covers it and a failure has
/// already ended the run, so a `compact` of `2` or a `deny` of `b.txt` stopped the program without
/// saying which file said so.
#[test]
fn a_value_out_of_a_file_is_refused_with_the_file_named() {
    // the two halves, and the kinds of value each of them refuses
    for (name, json, said) in [
        ("compact", r#"{ "compact": 2 }"#, "was `2`"),
        (
            "tools",
            r#"{ "tools": ["contxt"] }"#,
            "not one of this program's tools",
        ),
        (
            "rule",
            r#"{ "deny": ["b.txt"] }"#,
            "is read as a whole domain",
        ),
        (
            "servers",
            r#"{ "allow-server": ["nope"] }"#,
            "is not a server this run starts",
        ),
        (
            "device",
            r#"{ "sandbox-device": ["/home"] }"#,
            "names a device under `/dev`",
        ),
    ] {
        let dir = common::scratch(&format!("named-{name}"));
        let file = dir.join("kamchatka.json");
        std::fs::write(&file, json).expect("written");

        // found rather than named, which is the case that had nothing announced
        let (ok, out) = run_from(&dir, &[], "");
        assert!(
            !ok,
            "{name}: a value nobody can honour is not a success: {out}"
        );
        assert!(out.contains(said), "{name}: {out}");
        assert!(
            out.contains("kamchatka.json"),
            "{name}: the file is not named: {out}"
        );
        assert!(out.contains("Error:"), "{name}: it is a refusal: {out}");

        // and through `--config-file`, where the path is the one somebody typed
        let path = file.display().to_string();
        let (ok, out) = run(&["--config-file", &path], "");
        assert!(!ok, "{name}: {out}");
        assert!(out.contains(&path), "{name}: the file is not named: {out}");
    }
}

/// And a value somebody *typed* is refused on its own, because the file is not what is wrong.
///
/// note: the file is read either way and it stands for everything the command line did not say,
/// so an error naming it is true but beside the point - somebody looking at `--compact 2` is
/// looking at what they wrote.
#[test]
fn a_value_typed_on_the_command_line_is_not_answered_with_the_file() {
    let dir = common::scratch("typed-not-the-file");
    std::fs::write(dir.join("kamchatka.json"), r#"{ "model": null }"#).expect("written");

    let (ok, out) = run_from(&dir, &["--compact", "2"], "");

    assert!(!ok, "{out}");
    assert!(out.contains("was `2`"), "what was wrong: {out}");
    assert!(
        !out.contains("Error: kamchatka.json"),
        "a file that says nothing is named as though it had: {out}"
    );
}

/// A word the file gets wrong says what the word is for and what it can be.
///
/// note: clap's own answer to a `ValueEnum` is `invalid variant: maybe`, which is the name of a
/// Rust derive and not of anything a person writing a file has heard of - and a file has no
/// `--help` beside it. `deny` or `allow` are the two answers there are, and saying so is the
/// whole of what is missing.
#[test]
fn a_word_the_file_gets_wrong_says_what_it_can_be() {
    let path = settings("on-ask-choices", r#"{ "on-ask": "maybe" }"#);

    let (ok, said) = run(&["--config-file", &path], "");

    assert!(!ok, "a setting nobody can honour is not a success");
    assert!(said.contains("`maybe`"), "what it was: {said}");
    assert!(said.contains("`deny`"), "and what it can be: {said}");
    assert!(said.contains("`allow`"), "both of them: {said}");
    assert!(
        !said.contains("invalid variant"),
        "and not clap's word for a Rust derive: {said}"
    );
}

/// A file that is not an object says what a file is, rather than how many fields a struct has.
///
/// note: `[]` is what a hand-written file and an editor's bracket pair both produce first, and
/// serde's answer counts the fields of a struct nobody writing the file has heard of.
#[test]
fn a_settings_file_that_is_not_an_object_says_what_one_is() {
    let dir = common::scratch("not-an-object");
    std::fs::write(dir.join("kamchatka.json"), "[]").expect("written");

    let (ok, said) = run_from(&dir, &[], "");

    assert!(
        !ok,
        "a file this program cannot read is not a success: {said}"
    );
    assert!(
        said.contains("a settings file is an object"),
        "what a file is: {said}"
    );
    assert!(said.contains("kamchatka.json"), "and which: {said}");
}

/// A file name read as a domain says how to write it as a path rule.
///
/// note: a path rule is a file name in which `*` stands for any run of characters, matched against
/// the last name in a path - so `b.txt` is a rule about nothing and `b.txt*` is a rule about that
/// file. A bare name is read as a whole domain, which is what `files` and `shell` are too, so
/// nothing in the text can say which was meant; taking every bare name as a file is the other
/// reading and it is not this crate's to make. Saying how the rule is written is.
///
/// note: `src/main.rs` was answered by a different refusal - it *is* read as a path, and a rule
/// about a directory is that directory's name and a slash - which says the same thing.
#[test]
fn a_file_name_read_as_a_domain_says_how_to_write_the_rule() {
    use kamchatka::{
        tools::{Subject, objection_to, path_matches},
        wiring::Setup,
    };

    let checked = |rule: &str| {
        Setup {
            deny: vec![Subject::parse(rule)],
            ..Setup::default()
        }
        .check()
        .expect_err("a rule nothing is judged under is not a success")
    };

    for (rule, said) in [
        ("b.txt", "`b.txt*`"),
        ("main.rs", "`main.rs*`"),
        (
            "src/main.rs",
            "a rule about a directory is that directory's name and a slash",
        ),
    ] {
        let refusal = checked(rule);
        assert!(refusal.contains(rule), "{rule}: {refusal}");
        assert!(refusal.contains(said), "{rule}: {refusal}");
    }

    // a tool and a domain nothing declares are each still told what they are, so the refusal
    // above is not a new message for every wrong rule
    assert!(checked("shell").contains("judged as `exec:run`"));
    assert!(checked("network").contains("no call here is judged under"));

    // and the spellings the refusals give are rules the matcher takes
    assert!(path_matches("b.txt*", "b.txt"));
    assert!(path_matches("main.rs*", "main.rs"));
    assert_eq!(objection_to("b.txt*"), None);
    assert_eq!(objection_to("main.rs*"), None);
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

/// A build with no advisor refuses a file that turns it on.
///
/// note: only compiled where it is true. Accepted, the file's `advise` would be read by nothing,
/// and every question would go uncoloured with nothing saying why. `false` is the shipped file's
/// value, which `print_config_hands_over_a_file_this_program_would_accept` starts a session under.
#[cfg(not(feature = "shell-advisor"))]
#[test]
fn an_advisor_this_build_does_not_have_is_refused() {
    let path = settings("no-advisor", r#"{ "advise": true }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(!ok, "a setting nobody can honour is not a success");
    assert!(said.contains("no advisor in it"), "{said}");
}

/// A device named outside `/dev`, on the command line or in a file, is refused where it is given -
/// by where it leads, not how it is spelled.
///
/// note: a device is granted reading and writing whatever is beneath it, which is `--sandbox-allow`
/// anywhere else, and a writable path the screens never mention.
#[test]
fn a_device_outside_dev_is_refused() {
    let (ok, said) = run(&["--sandbox-device", "/tmp/not-a-device"], "");
    assert!(!ok, "{said}");
    assert!(said.contains("names a device under `/dev`"), "{said}");

    let path = settings("device-outside", r#"{ "sandbox-device": ["/home"] }"#);
    let (ok, said) = run(&["--config-file", &path], "");
    assert!(!ok, "{said}");
    assert!(said.contains("names a device under `/dev`"), "{said}");

    // begins with `/dev` by its components, and is `/tmp` once the `..` is settled
    let (ok, said) = run(&["--sandbox-device", "/dev/../tmp"], "");
    assert!(!ok, "{said}");
    assert!(said.contains("names a device under `/dev`"), "{said}");
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
/// the conversation as well as onto a stream a screen is about to cover.
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

/// A file underfoot is said before a server it names is started, and not only once a session is.
///
/// note: the server fails its handshake, so the program stops before there is a conversation to
/// say anything in - and a file that could start programs has been read by then.
#[cfg(feature = "mcp")]
#[test]
fn a_file_underfoot_is_said_before_its_servers_start() {
    let dir = common::scratch("underfoot-server");
    std::fs::write(dir.join("kamchatka.json"), r#"{ "mcp": ["broken=false"] }"#)
        .expect("a settings file where the program will stand");

    let (ok, said) = run_from(&dir, &[], "");

    assert!(!ok, "a server that never answers stops the run: {said}");
    assert!(said.contains("settings read from kamchatka.json"), "{said}");
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
    let out = Command::new(common::program())
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
    let out = Command::new(common::program())
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

/// `--help` names the variables the program reads, and the advisor's only in a build with an
/// `--advise` to use them.
///
/// note: they are read rather than declared as arguments, so clap lists none of them on its own,
/// and a variable nothing on the screen mentions is one nobody finds.
#[test]
fn help_names_the_variables_the_program_reads() {
    let out = Command::new(common::program())
        .arg("--help")
        .output()
        .expect("the binary under test is built");
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).expect("help is text");

    for variable in [
        "KAMCHATKA_API_KEY",
        "KAMCHATKA_BASE_URL",
        "KAMCHATKA_CONTEXT_LIMIT",
        "KAMCHATKA_NO_ATTRIBUTION",
    ] {
        assert!(help.contains(variable), "no {variable}: {help}");
    }
    // the two of the advisor's four that `--advise`'s own line does not name
    for variable in ["KAMCHATKA_SYSTEM1_BASE_URL", "KAMCHATKA_SYSTEM1_MODEL"] {
        assert_eq!(
            help.contains(variable),
            cfg!(feature = "shell-advisor"),
            "{variable}: {help}"
        );
    }
}

/// A base URL that is not an address is refused at startup, by the variable's name, in either
/// dialect.
///
/// note: it went unchecked until the first request, which failed as a `builder error` that named
/// neither the variable nor what it held.
#[test]
fn a_base_url_that_is_not_an_address_is_refused_at_startup() {
    for args in [&["--headless"][..], &["--headless", "--gemini"]] {
        let (ok, said) = run_with(args, "hi\n", &[("KAMCHATKA_BASE_URL", "not-a-url")]);
        assert!(!ok, "{args:?}: {said}");
        assert!(
            said.contains("KAMCHATKA_BASE_URL is `not-a-url`"),
            "{args:?}: {said}"
        );
    }
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

/// `/save` and `/load` agree on an argument that is only a suffix, and `/load` says when it was
/// given a directory.
///
/// note: `/save .json` took the suffix off and was left with nothing, and wrote `.json` and
/// `.jsonl` - files `ls` does not show, under a confirmation that read as though it had worked.
/// `/load rec/` after `/save rec/` answered that it could not read `rec/.json`.
#[test]
fn save_and_load_agree_on_a_bare_suffix_and_a_directory() {
    let dir = common::scratch("save-suffix");
    std::fs::create_dir(dir.join("rec")).expect("a directory");
    let (ok, said) = run_from(
        &dir,
        &[],
        "remember 4817\n/save .json\n/load .jsonl\n/load rec/\n",
    );
    assert!(ok, "{said}");

    assert!(dir.join("session.json").exists(), "{said}");
    assert!(dir.join("session.jsonl").exists(), "{said}");
    assert!(!dir.join(".json").exists() && !dir.join(".jsonl").exists());
    assert!(
        said.contains("(session.json)"),
        "`/load` reads it back: {said}"
    );
    // and a directory, which `/save` writes into by the session's name, is said to be one
    assert!(said.contains("rec/ is a directory"), "{said}");
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

/// A rule about something no call here is judged under is refused where it is given, and says
/// what there is.
///
/// note: `--deny shell` parsed as a domain called `shell`, and the shell is judged as `exec:run`, so
/// it refused nothing - a headless run given `--on-ask allow` then ran every command unasked, under
/// a rule that read as given. A typo in an operation was the same silence.
#[test]
fn a_rule_nothing_is_judged_under_is_refused() {
    use kamchatka::{tools::Subject, wiring::Setup};

    let checked = |rule: &str| {
        Setup {
            deny: vec![Subject::parse(rule)],
            ..Setup::default()
        }
        .check()
    };

    let tool = checked("shell").expect_err("a tool's name is not what it is judged as");
    assert!(tool.contains("`exec:run`"), "{tool}");
    let typo = checked("fs:writ").expect_err("an operation nothing does");
    assert!(typo.contains("write"), "{typo}");
    let nowhere = checked("network").expect_err("a domain nothing is judged under");
    assert!(nowhere.contains("exec"), "{nowhere}");

    for rule in [
        "exec",
        "exec:run",
        "fs:write",
        "net:reach",
        "mcp:call",
        "*.pem",
    ] {
        assert!(
            checked(rule).is_ok(),
            "`{rule}` is a rule something is judged under"
        );
    }
}

/// A server rule naming no server this run starts is refused where it is given, and says which
/// servers there are.
///
/// note: `--deny-server filess` beside `--mcp files=...` refused nothing, and a headless run given
/// `--on-ask allow` ran every call to `files` unasked, under a rule that read as given.
#[cfg(feature = "mcp")]
#[test]
fn a_server_rule_naming_no_server_is_refused() {
    let refused = "is not a server this run starts";

    let (ok, said) = run(&["--mcp", "files=/nowhere", "--deny-server", "filess"], "");
    assert!(!ok, "{said}");
    assert!(
        said.contains(&format!("`filess` {refused}; they are files")),
        "{said}"
    );
    let (ok, said) = run(&["--allow-server", "files"], "");
    assert!(!ok, "{said}");
    assert!(said.contains("it starts none"), "{said}");

    // a server with no name of its own is called after its program, as `attach` calls it
    let (_, said) = run(
        &[
            "--mcp",
            "/nowhere/files --root /srv",
            "--deny-server",
            "files",
        ],
        "",
    );
    assert!(!said.contains(refused), "{said}");
}

/// An `allow` for `mcp:call` or `mcp` is refused with a pointer to `--allow-server`, and a `deny`
/// of either is kept.
///
/// note: a server's tools are judged under its name in place of `mcp:call`, so `--allow mcp:call`
/// beside `--mcp foreign=...` read as given and every call to `foreign` still went to the question.
#[test]
fn an_allow_for_mcp_is_refused_and_a_deny_is_not() {
    for rule in ["mcp", "mcp:call"] {
        let (ok, said) = run(&["--allow", rule], "");
        assert!(!ok, "{said}");
        assert!(said.contains("`--allow-server NAME`"), "{said}");

        let (_, said) = run(&["--deny", rule], "");
        assert!(!said.contains("grants nothing"), "{said}");
    }
}

/// A settings file's servers are the ones a run starts.
///
/// note: read off a server rule's refusal, which names the servers there are, so that nothing has
/// to be spawned to see which list arrived.
#[cfg(feature = "mcp")]
#[test]
fn a_settings_file_names_the_servers_a_run_starts() {
    let path = settings("servers", r#"{ "mcp": ["files=/nowhere"] }"#);

    let (ok, said) = run(&["--config-file", &path, "--deny-server", "filess"], "");

    assert!(!ok, "{said}");
    assert!(said.contains("they are files"), "{said}");
}

/// A typed `--advise` is not turned off by a file saying `false`.
///
/// note: read off the refusal a session asked to advise gives when it has no advisor to reach,
/// which comes before anything is sent.
#[cfg(feature = "shell-advisor")]
#[test]
fn a_typed_advise_beats_the_file() {
    let path = settings("advise-off", r#"{ "advise": false }"#);

    let (ok, said) = run(&["--config-file", &path, "--advise"], "");

    assert!(!ok, "{said}");
    assert!(said.contains("--advise needs a key"), "{said}");
}

/// `--advise` puts the advisor in front of the standing rules, and a session that did not ask for
/// it has the rules alone.
///
/// note: a local advisor, so that nothing leaves the machine: `SYSTEM1_ADVISOR_COMMAND` is read
/// before any key, and `true` is enough for a session that asks nothing. What is read is the
/// policy the record names when the session starts, since a rating is only ever drawn beside a
/// question.
#[cfg(feature = "shell-advisor")]
#[test]
fn an_advisor_asked_for_is_the_policy_the_session_runs_under() {
    let local = [("SYSTEM1_ADVISOR_COMMAND", "true")];

    let (ok, said) = run_with(&["--advise"], "", &local);
    assert!(ok, "{said}");
    assert!(said.contains("tools::advice::Advised"), "{said}");

    let (ok, said) = run_with(&[], "", &local);
    assert!(ok, "{said}");
    assert!(!said.contains("tools::advice::Advised"), "{said}");
}

/// `--spend 0` is no ceiling, as `/spend 0` and `--requests 0` are.
///
/// note: it was a ceiling of nothing, reached before the first request: a headless run read no
/// lines and ended without saying why.
#[test]
fn a_spend_of_nothing_is_no_ceiling() {
    let (ok, said) = run(&["--spend", "0"], "/spend\n");
    assert!(ok, "{said}");
    assert!(said.contains("no ceiling"), "{said}");

    let path = settings("spend-zero", r#"{ "spend": 0 }"#);
    let (ok, said) = run(&["--config-file", &path], "/spend\n");
    assert!(ok, "{said}");
    assert!(said.contains("no ceiling"), "{said}");
}

/// A snapshot whose name is a path is not carried on from, because the name is where its record
/// would be written.
///
/// note: resumed, `../../escaped` was the stem the record went under, and the log and the snapshot
/// were written two directories up from the private one they belong in.
#[test]
fn a_snapshot_named_with_a_path_is_not_carried_on_from() {
    let dir = common::scratch("resumed-named-a-path");
    let (ok, said) = run_from(&dir, &[], "/save first.json\n");
    assert!(ok, "{said}");

    let mut snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("first.json")).expect("saved"))
            .expect("a snapshot");
    snapshot["session"] = json!("../../escaped");
    std::fs::write(dir.join("escaped.json"), snapshot.to_string()).expect("written");

    let (ok, said) = run_from(&dir, &["-r", "escaped.json"], "");
    assert!(!ok, "{said}");
    assert!(said.contains("../../escaped"), "it says which: {said}");
}

/// A read-only path inside the working directory is refused, because nothing could hold it
/// read-only: the shell's confinement only ever adds to what it may do.
#[test]
fn a_read_only_path_inside_the_working_directory_is_refused() {
    let dir = common::scratch("read-only-inside");
    std::fs::create_dir_all(dir.join("protected")).expect("a directory");

    let (ok, said) = run_from(&dir, &["--sandbox-read", "protected"], "/tools\n");
    assert!(!ok, "{said}");
    assert!(said.contains("--sandbox-read"), "{said}");

    // not there yet, and as writable once it is
    let (ok, said) = run_from(&dir, &["--sandbox-read", "not-yet"], "/tools\n");
    assert!(!ok, "{said}");
    assert!(said.contains("--sandbox-read"), "{said}");
}

/// A `~` in a path a person typed is their home directory, in every command that takes one.
///
/// note: the settings file expanded one because nothing was in front of it, and `/attach` did not,
/// so a path under the home directory was read out of a file and refused when the same path was
/// typed at the prompt. The prompt has no shell in front of it either, which is the whole of the
/// argument, and the three commands are the ones a person types a path into.
///
/// note: the tools still refuse a `~`, on purpose: those paths are written by a model. This is
/// about what the person who owns the home directory types, and the two are told apart by where
/// the path came from rather than by the shape of the path.
///
/// note: a home of the test's own, over the top of whatever the machine has. `config::home` reads
/// `HOME` and nothing else - a password database's answer and the one the person is working from
/// are allowed to differ, and a path that quietly goes somewhere else is worse than one that
/// fails - so setting it is how a test says which home it means.
#[test]
fn a_tilde_typed_at_the_prompt_is_the_home_directory() {
    let home = common::scratch("typed-tilde-home");
    std::fs::write(home.join("notes.txt"), "PLUM").expect("a file in it");
    let home = home.display().to_string();

    // `/attach`, which is the one the guide shows a `~` in
    let (ok, said) = run_with(&[], "/attach ~/notes.txt\n", &[("HOME", &home)]);
    assert!(ok, "{said}");
    assert!(said.contains("went into the context"), "{said}");
    assert!(
        said.contains(&format!("{home}/notes.txt")),
        "the path that went in was not the home directory's: {said}"
    );

    // and `/save`, which writes rather than reads and used to make a directory called `~` under
    // wherever the session was standing
    let (ok, said) = run_with(&[], "/save ~/session\n", &[("HOME", &home)]);
    assert!(ok, "{said}");
    assert!(
        said.contains(&format!("{home}/session.json")),
        "the save did not land in the home directory: {said}"
    );
    assert!(
        Path::new(&home).join("session.json").is_file(),
        "and nothing was written there"
    );

    // a `~` that is somebody else's is left as it was typed: resolving another user's home means
    // asking the password database, and a path that quietly is not what it says is worse than one
    // that is obviously wrong
    let (ok, said) = run_with(&[], "/attach ~root/.ssh\n", &[("HOME", "/nowhere")]);
    assert!(!said.contains("went into the context"), "{said}");
    assert!(!ok || said.contains("~root/.ssh"), "{said}");
}

/// A `KAMCHATKA_CONTEXT_LIMIT` that is not a number of tokens is refused at startup, by name.
///
/// note: a value that did not parse used to read as *no limit at all*, so a mistyped figure - or
/// one written where a shell variable's name was wanted - left a session measuring against
/// whatever the endpoint advertises with nothing on the screen saying the setting was not in
/// force. A `0` was worse still: it is a limit, and the first request is refused against it, with
/// the budget reading `the limit: 0, which the next request would fill 0.0% of`.
///
/// note: refused at startup rather than at the first turn, the way every other figure this
/// program settles is. The environment is the one place a figure can be wrong with no file to
/// open, so the sentence names the variable and says what it held.
#[test]
fn a_context_limit_that_is_not_a_number_of_tokens_is_refused() {
    for wrong in ["abc", "-5", "1.5", ""] {
        for args in [&["--headless"][..], &["--headless", "--gemini"]] {
            let (ok, said) = run_with(args, "", &[("KAMCHATKA_CONTEXT_LIMIT", wrong)]);
            assert!(!ok, "`{wrong}` was taken as a limit: {args:?}: {said}");
            assert!(
                said.contains("KAMCHATKA_CONTEXT_LIMIT"),
                "the variable is not named: {said}"
            );
            assert!(
                said.contains(&format!("`{wrong}`")),
                "what it held is not said back: {said}"
            );
        }
    }

    // and `0` is refused with its own sentence rather than the one above, because a whole number
    // above 0 and a whole number of nothing are two different mistakes
    let (ok, said) = run_with(&[], "", &[("KAMCHATKA_CONTEXT_LIMIT", "0")]);
    assert!(!ok, "`0` was taken as a limit: {said}");
    assert!(said.contains("KAMCHATKA_CONTEXT_LIMIT"), "{said}");
    assert!(
        said.contains("how many the model holds"),
        "and not told what the figure is for: {said}"
    );

    // a whole number above 0 is taken, which is what the variable is for. A model is named
    // because a session without one holds no provider, and there is no limit for `/budget` to read
    let (ok, said) = run_with(
        &["-m", "a-model"],
        "/model\n",
        &[("KAMCHATKA_CONTEXT_LIMIT", "128000")],
    );
    assert!(ok, "{said}");
    assert!(said.contains("128,000"), "{said}");

    // and answered before the key is: the figure is in the environment and costs nothing to read,
    // so a run with no key at all is told about the setting rather than about the credential.
    // `spawn` is reached directly because `run_keyless` is the one helper with no environment
    // over the top of it, and this is the one case that needs both
    let (ok, said) = spawn(
        elsewhere(),
        &[],
        "",
        &[("KAMCHATKA_CONTEXT_LIMIT", "abc")],
        false,
    );
    assert!(!ok, "{said}");
    assert!(
        said.contains("KAMCHATKA_CONTEXT_LIMIT"),
        "the setting was not reported: {said}"
    );
    assert!(
        !said.contains("KAMCHATKA_API_KEY"),
        "the key is reported instead of the setting: {said}"
    );
}

/// A file `-f` could not read says why, once, the way `/attach` says it.
///
/// note: the wiring named the file itself and then formatted the error with `{e}`, which drops
/// anything anyhow put underneath - so a missing file came out as `missing.png: could not read
/// missing.png`: the same path twice and no word about why. A pipe came out as `fifo: fifo is not
/// a file`, which reads as a complaint about the spelling. The prompt's own `/attach` already
/// prints the whole chain with `{e:#}`, and the two acts are the same act, so they read alike.
#[test]
fn a_file_named_on_the_command_line_says_why_it_could_not_be_read() {
    let dir = common::scratch("file-refused");
    std::fs::write(dir.join("notes.md"), "notes").expect("a file that is there");

    let (ok, said) = run_from(&dir, &["-f", "missing.png"], "");
    assert!(!ok, "a file that is not there is not a success: {said}");
    // the path once, and the operating system's own account of why
    assert_eq!(
        said.matches("missing.png").count(),
        1,
        "the file is named twice: {said}"
    );
    assert!(
        said.contains("No such file or directory"),
        "and nothing says why: {said}"
    );

    // and a thing that is there and is not a file says so once rather than twice as well
    std::fs::create_dir(dir.join("adir")).expect("a directory");
    let (ok, said) = run_from(&dir, &["-f", "adir"], "");
    assert!(!ok, "{said}");
    assert_eq!(
        said.matches("adir").count(),
        1,
        "the directory is named twice: {said}"
    );
    assert!(said.contains("is not a file"), "{said}");

    // and one that is there goes in
    let (ok, said) = run_from(&dir, &["-f", "notes.md"], "/budget\n");
    assert!(ok, "{said}");
}
