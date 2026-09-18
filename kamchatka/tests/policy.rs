//! Tests for the permission policy's own rules: what a pattern matches, and what the policy
//! answers about a call because of it.
//!
//! note: these were only ever exercised through the screen, which draws the answer rather than
//! asking for it. A rule that silently fails to match draws nothing and looks exactly like a rule
//! nobody tripped - which is how `.env/` came to be a way of reading `.env` without being asked
//! about it.

use std::path::PathBuf;

use kamchatka::{
    sandbox::{Access, Reach},
    tools::{Careful, Subject, domains, objection_to, path_matches},
};
use nachalnik::{
    Capability, Domain, PermissionId, PermissionPolicy, PermissionRequest, ToolCall, ToolCallId,
    Verdict,
};
use serde_json::json;

mod common;

/// What the policy would answer about `read`ing this path.
fn asking_about(policy: &Careful, path: &str) -> Verdict {
    let call = ToolCall::new("c1", "read", json!({ "path": path }));
    let request = PermissionRequest {
        id: PermissionId(1),
        call: call.id.clone(),
        tool: "read".to_owned(),
        capabilities: vec![Capability::fs("read")],
        args: call.args.clone(),
    };

    policy.verdict(&request)
}

/// The rule has to be about the file that will actually be opened, whatever spelling of it the
/// model produced. A pattern is matched against a name and the file is opened at a *resolved*
/// path, and the two used to disagree about the simplest thing there is: a trailing slash.
#[test]
fn a_credential_rule_is_about_the_file_that_gets_opened() {
    let dir = common::scratch("policy");
    std::fs::create_dir_all(dir.join("sub")).expect("a temporary directory");
    std::fs::write(dir.join(".env"), "TOKEN=hunter2").expect("something worth protecting");
    // note: a temporary directory is usually reached by a name that is not where it is - `/var` is
    // a symlink to `/private/var` on macOS, and Windows hands out an 8.3 short name for a profile
    // directory. What `allows` answers with is the path the file will be opened at, so that is the
    // name to expect it under. The reach is handed the name as it came, which is the one a person
    // would have typed, and on Windows is the only form of it that `..` and a forward slash still
    // mean anything in: a `\\?\` path goes to the system unnormalized.
    let opened_at = dir.canonicalize().expect("the directory was just made");

    let reach = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };
    // the moment the credential list exists for: somebody has answered `always` to an ordinary
    // read, so the capability no longer asks and the rule is the only thing left standing
    let policy = Careful::new();
    policy.set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);

    for spelling in [".env", "./.env", ".env/", ".env//", "sub/../.env", ".env/."] {
        assert_eq!(
            reach.allows(spelling, Access::Reading).as_deref(),
            Ok(opened_at.join(".env").as_path()),
            "`{spelling}` is a way of naming the same file"
        );
        assert!(
            path_matches(".env*", spelling),
            "`{spelling}` opens .env, so the rule about .env has to be about it"
        );
        assert_eq!(
            asking_about(&policy, spelling),
            Verdict::Ask,
            "`{spelling}` went through without a question"
        );
    }

    // and an ordinary file is not a question, which is what answering `always` bought
    assert_eq!(asking_about(&policy, "sub/notes.txt"), Verdict::Allow);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `*` stands for any run of characters, and the matcher backtracks to find one.
#[test]
fn a_pattern_finds_a_match_where_there_is_one() {
    // the case the first version got wrong: it took the first `bc` it found and had no way back
    assert!(path_matches("a*bc", "abcbc"));
    assert!(path_matches("*credentials*.json", "credentials.json"));
    assert!(path_matches("*.pem*.pem", "a.pem.b.pem"));

    for (pattern, name) in [
        (".env*", ".env"),
        (".env*", ".env.local"),
        ("*.pem", "key.pem"),
        ("id_rsa*", "id_rsa.pub"),
        ("*credentials*", "aws-credentials.json"),
        ("*", "anything"),
        ("exact", "exact"),
    ] {
        assert!(path_matches(pattern, name), "{pattern} should match {name}");
    }

    for (pattern, name) in [
        (".env*", "env"),
        ("*.pem", "a.pem.txt"),
        ("*.pem", "pem"),
        ("exact", "exactly"),
        ("a*bc", "abcb"),
    ] {
        assert!(
            !path_matches(pattern, name),
            "{pattern} should not match {name}"
        );
    }
}

/// A pattern ending in `/` is about a directory anywhere in the path.
#[test]
fn a_directory_rule_is_about_a_component() {
    for path in [".ssh/id_rsa", "/home/somebody/.ssh/id_rsa", "a/.ssh/./b"] {
        assert!(path_matches(".ssh/", path), "{path}");
    }
    for path in ["notes/id_rsa", "sshkeys/x", ".sshx/y"] {
        assert!(!path_matches(".ssh/", path), "{path}");
    }
}

/// Every rule this takes is one the matcher can match, and the ones it cannot are refused by name.
///
/// note: `--allow 'src/**'` was taken, drawn on the permissions tab and consulted about every
/// call, and no path has ever matched it - it has no trailing slash, so it was compared with file
/// names, which hold no `/`. RUNNING.md offered it as the example of a path rule. The pair that
/// matters is the two halves agreeing: what is accepted is what `path_matches` can answer yes to.
#[test]
fn a_rule_that_could_never_match_is_refused_rather_than_kept() {
    for (pattern, matching) in [
        ("*.pem", "/home/x/key.pem"),
        (".env*", "/srv/app/.env.local"),
        ("id_rsa*", "/home/x/.ssh/id_rsa"),
        ("secrets/", "/srv/secrets/token"),
        (".ssh/", "/home/x/.ssh/config"),
    ] {
        assert_eq!(
            objection_to(pattern),
            None,
            "`{pattern}` is a rule and was refused"
        );
        assert!(
            path_matches(pattern, matching),
            "`{pattern}` was taken and does not match `{matching}`"
        );
    }

    for pattern in [
        "src/**",
        "src/*.rs",
        "secrets/*.key",
        "a\\b",
        "/",
        "",
        "secrets*/",
    ] {
        let objection = objection_to(pattern)
            .unwrap_or_else(|| panic!("`{pattern}` was taken and nothing can match it"));
        assert!(
            objection.contains(pattern),
            "the objection names it: {objection}"
        );
        assert!(
            objection.contains("`*.pem`") && objection.contains("`secrets/`"),
            "and says what there is: {objection}"
        );
    }
}

/// Every one of this program's own rules is one it would accept from somebody else.
#[test]
fn the_rules_it_ships_with_are_rules_it_would_take() {
    let policy = Careful::new();
    for (pattern, _) in policy.paths() {
        assert_eq!(
            objection_to(&pattern),
            None,
            "`{pattern}` is shipped and would be refused"
        );
    }
}

/// The sandbox is the boundary that does not care about names; these rules do, and say so.
#[test]
fn the_rules_are_about_names_and_a_symlink_is_not_one() {
    let dir = common::scratch("policy-link");
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    std::fs::write(dir.join(".env"), "TOKEN=hunter2").expect("something worth protecting");

    // a name with no rule about it, pointing at a file there is one about
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join(".env"), dir.join("notes.txt")).expect("a symlink");
        // the same two names for one directory as above: the reach is given the one it was
        // reached by, and answers with the one the file is at
        let opened_at = dir.canonicalize().expect("the directory was just made");

        assert!(
            !path_matches(".env*", "notes.txt"),
            "a rule is about a name, and this is a different name"
        );
        // it is still inside the working directory, so the reach allows it - which is the honest
        // shape of a name rule and the reason the sandbox is where the boundary is
        let reach = Reach {
            workdir: dir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: true,
        };
        assert_eq!(
            reach.allows("notes.txt", Access::Reading).as_deref(),
            Ok(opened_at.join(".env").as_path()),
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Nothing outside the working directory, however it is spelled.
#[test]
fn the_reach_refuses_what_is_outside_it() {
    let dir = common::scratch("policy-reach");
    std::fs::create_dir_all(dir.join("sub")).expect("a temporary directory");
    let reach = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };

    for outside in ["/etc/passwd", "../outside.txt", "sub/../../outside.txt"] {
        assert!(reach.allows(outside, Access::Reading).is_err(), "{outside}");
    }
    // including one that does not exist yet, which is most of what `write` is handed
    assert!(reach.allows("sub/../../new.txt", Access::Reading).is_err());
    assert!(reach.allows("sub/new.txt", Access::Reading).is_ok());

    // and with the confinement off, nothing is refused
    let open = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: false,
    };
    assert_eq!(
        open.allows("/etc/passwd", Access::Reading),
        Ok(PathBuf::from("/etc/passwd"))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A batch's answers all stand until the batch is over, and then they go together.
///
/// note: they were bounded at sixty-four and the oldest went, which throws away the entry most
/// likely to be wanted: every call in a batch is decided before any of them runs, so the
/// sixty-fifth `yes` in one response dropped the first, and that command ran with the network cut
/// after somebody had allowed it. The bound is gone and the lifetime is the batch; `App` empties
/// it at the next request, which is the one moment nothing can still be waiting for one.
#[test]
fn every_answer_in_a_batch_stands_until_the_batch_is_over() {
    let policy = Careful::new();
    policy.set(&Subject::parse("exec:run"), Verdict::Allow);

    // more granted calls than any bound this used to have, oldest first
    for n in 0..200 {
        policy.grant_the_network(&ToolCallId::from(format!("call_{n}").as_str()));
    }
    assert!(
        policy.was_granted_the_network(&ToolCallId::from("call_0")),
        "the first answer of a batch is the one a bound dropped"
    );
    assert!(policy.was_granted_the_network(&ToolCallId::from("call_199")));

    // saying the same one twice is not two of them
    for _ in 0..200 {
        policy.grant_the_network(&ToolCallId::from("call_199"));
    }
    assert!(policy.was_granted_the_network(&ToolCallId::from("call_198")));

    policy.forget_network_grants();
    assert!(
        !policy.was_granted_the_network(&ToolCallId::from("call_199")),
        "a one-off answer outlived the batch it was given in"
    );
    assert_eq!(
        policy.stance(&Subject::parse("exec:run")),
        Verdict::Allow,
        "and a rule somebody wrote is not a one-off answer"
    );
}

/// The same for the refusals, which is where it bites: a refusal the model reads is written down
/// when the policy answers and read when the kernel builds the tool result.
#[tokio::test]
async fn a_refusal_is_still_accounted_for_after_a_great_many_of_them() {
    let policy = Careful::new();
    policy.set(
        &Subject::Capability(Capability::net("reach")),
        Verdict::Deny,
    );

    for n in 0..200 {
        let call = ToolCall::new(
            format!("call_{n}"),
            "shell",
            json!({ "cmd": "curl https://example.com" }),
        );
        let request = PermissionRequest {
            id: PermissionId(n),
            call: call.id.clone(),
            tool: "shell".to_owned(),
            capabilities: vec![Capability::exec("run")],
            args: call.args.clone(),
        };
        assert_eq!(policy.evaluate(&request).await, Verdict::Deny);
    }

    let latest = policy
        .why(&ToolCallId::from("call_199"))
        .expect("the refusal just made is the one the model is about to read");
    assert!(latest.contains("net:reach"), "{latest}");
    assert_eq!(policy.why(&ToolCallId::from("call_0")), None);
}

/// What the policy would answer about a call doing this to the context.
///
/// note: one operation, which is what a tool reports through `Tool::needs`. The policy no longer
/// reads the `action` argument itself - it is handed what the call needs and answers about that.
fn asking_about_action(policy: &Careful, action: &str) -> Verdict {
    let call = ToolCall::new(
        "c1",
        "context",
        json!({ "action": action, "reason": "why" }),
    );
    let request = PermissionRequest {
        id: PermissionId(1),
        call: call.id.clone(),
        tool: "context".to_owned(),
        capabilities: vec![domains::context(action)],
        args: call.args.clone(),
    };

    policy.verdict(&request)
}

/// `context` allows the context: every action it has, with nothing else to say about it.
///
/// note: four of them were seeded as questions, so allowing the tool left an `exclude` still
/// asking - which made `--allow context` mean something other than `context`, with no way to tell
/// from the words which four were the exceptions.
#[test]
fn allowing_a_tool_allows_every_action_of_it() {
    let policy = Careful::new();
    policy.set(&Subject::parse("context"), Verdict::Allow);

    for action in [
        "note", "pin", "restore", "undo", "elide", "exclude", "revise",
    ] {
        assert_eq!(
            asking_about_action(&policy, action),
            Verdict::Allow,
            "`context` is allowed, so `{action}` is"
        );
    }
}

/// And naming one action allows that one, with nothing else said about the tool.
#[test]
fn allowing_one_action_allows_only_that_action() {
    let policy = Careful::new();
    policy.set(&Subject::parse("context:note"), Verdict::Allow);

    assert_eq!(asking_about_action(&policy, "note"), Verdict::Allow);
    for action in ["elide", "exclude", "revise"] {
        assert_eq!(
            asking_about_action(&policy, action),
            Verdict::Ask,
            "only `note` was allowed, so `{action}` is still a question"
        );
    }
    assert_eq!(
        policy.stance(&Subject::parse("context")),
        Verdict::Ask,
        "and the tool itself was never answered about"
    );
}

/// An action rule narrows an allowed tool, which is the other half of naming one.
#[test]
fn refusing_one_action_refuses_only_that_action() {
    let policy = Careful::new();
    policy.set(&Subject::parse("context"), Verdict::Allow);
    policy.set(&Subject::parse("context:revise"), Verdict::Deny);

    assert_eq!(asking_about_action(&policy, "revise"), Verdict::Deny);
    assert_eq!(asking_about_action(&policy, "note"), Verdict::Allow);
}

/// A tool somebody refused stays refused, however finely an action of it is named.
///
/// note: the half that keeps `--deny` worth writing. There is deliberately no way to spell "the
/// tool is refused, but this one action is fine": the strictest of everything consulted wins, and
/// `--deny` is the last word.
#[test]
fn an_action_rule_cannot_loosen_a_tool_that_was_refused() {
    let policy = Careful::new();
    policy.set(&Subject::parse("context"), Verdict::Deny);
    policy.set(&Subject::parse("context:elide"), Verdict::Allow);

    assert_eq!(asking_about_action(&policy, "elide"), Verdict::Deny);
}

/// A rule is spelled the way it is read out, so `--deny "$(a row off the permissions tab)"` means
/// what it says - which is the property `Subject::parse` exists to keep.
#[test]
fn every_kind_of_rule_survives_the_trip_through_text() {
    for (text, expected) in [
        (
            "context:revise",
            Subject::Capability(domains::context("revise")),
        ),
        ("fs:read", Subject::Capability(Capability::fs("read"))),
        ("exec:run", Subject::Capability(Capability::exec("run"))),
        ("fs", Subject::Domain(Domain::Fs)),
        (
            "context",
            Subject::Domain(Domain::Other("context".to_owned())),
        ),
        (".env*", Subject::Path(".env*".to_owned())),
    ] {
        let subject = Subject::parse(text);
        assert_eq!(subject, expected, "`{text}` read wrongly");
        assert_eq!(subject.to_string(), text, "`{text}` wrote back differently");
        assert_eq!(Subject::parse(&subject.to_string()), subject);
    }
}

/// The policy is handed what a call needs and answers about that, without reading the arguments
/// for anything but a path.
///
/// note: it used to build a second subject out of the `action` argument and the tool's name,
/// which meant deciding from a string's shape whether `<name>:<name>` was an operation or a tool
/// with a colon in its name. What a call needs is the tool's to say - see `Tool::needs` - so
/// there is one subject here and the policy never guesses.
#[test]
fn the_policy_is_told_what_a_call_needs_rather_than_reading_it() {
    let policy = Careful::new();
    policy.set(&Subject::parse("context"), Verdict::Allow);

    let call = ToolCall::new("c1", "context", json!({ "action": "look" }));
    let request = PermissionRequest {
        id: PermissionId(1),
        call: call.id.clone(),
        tool: "context".to_owned(),
        capabilities: vec![domains::context("look")],
        args: call.args.clone(),
    };

    assert_eq!(
        policy.judges(&request),
        vec![Subject::Capability(domains::context("look"))]
    );
    assert_eq!(policy.verdict(&request), Verdict::Allow);
}

/// `always` answers for everything the policy consulted, and what it consulted is the grain of
/// the answer: the tool, unless somebody has written a rule finer than one.
#[test]
fn always_answers_at_the_grain_the_rules_are_written_at() {
    let asked_about = |action: &str| {
        let call = ToolCall::new(
            "c1",
            "context",
            json!({ "action": action, "reason": "why" }),
        );
        PermissionRequest {
            id: PermissionId(1),
            call: call.id.clone(),
            tool: "context".to_owned(),
            capabilities: vec![domains::context(action)],
            args: call.args.clone(),
        }
    };

    // the question was about one operation, so that is what the answer is about, and the rest of
    // the domain is untouched by it
    let policy = Careful::new();
    let request = asked_about("exclude");
    policy.always(&policy.judges(&request));
    assert_eq!(policy.verdict(&request), Verdict::Allow);
    assert_eq!(asking_about_action(&policy, "revise"), Verdict::Ask);

    // and a rule about an action it was not asked about is not answered by it, the way a
    // credential path survives an `always` for `read`
    let policy = Careful::new();
    policy.set(&Subject::parse("context:revise"), Verdict::Deny);
    let request = asked_about("exclude");
    policy.always(&policy.judges(&request));
    assert_eq!(policy.verdict(&request), Verdict::Allow);
    assert_eq!(
        asking_about_action(&policy, "revise"),
        Verdict::Deny,
        "`revise` was never consulted, so nothing here answered it"
    );
}

/// The two arguments the policy reads are found inside the wrapper the schema puts them in.
///
/// note: the regression this is here for, and it is the worse half of the one that had `context`
/// asking permission for all thirteen of its operations. These tools take their arguments inside a
/// `call` object; `judges` was reading the outside of one, where there is no `cmd` and no `path`.
/// So a `curl` stopped being judged against `net:reach` and every path rule stopped matching, both
/// silently and both in the direction of allowing more - which is the one direction a permission
/// policy does not get to fail in.
///
/// note: flat as well as wrapped, because somebody else's tool arrives through an MCP server with
/// no wrapper at all and is judged by the same code.
#[test]
fn the_policy_reads_a_path_and_a_command_through_the_wrapper() {
    let policy = Careful::new();
    policy.set(&Subject::parse(".env*"), Verdict::Deny);

    let ask = |tool: &str, capability: Capability, args: serde_json::Value| PermissionRequest {
        id: PermissionId(1),
        call: nachalnik::ToolCallId("c1".to_owned()),
        tool: tool.to_owned(),
        capabilities: vec![capability],
        args: std::sync::Arc::new(args),
    };

    for wrap in [true, false] {
        let dress = |args: serde_json::Value| match wrap {
            true => json!({ "call": args }),
            false => args,
        };

        // a command whose whole point is the network answers to `net:reach` as well as to running
        let curl = ask(
            "shell",
            Capability::exec("run"),
            dress(json!({ "action": "run", "cmd": "curl https://example.com" })),
        );
        assert!(
            policy
                .judges(&curl)
                .contains(&Subject::Capability(Capability::net("reach"))),
            "wrapped={wrap}: a `curl` was not judged against the network"
        );

        // and a path there is a rule about is one of the things the call is judged by
        let secret = ask(
            "fs",
            Capability::fs("read"),
            dress(json!({ "action": "read", "path": ".env.local" })),
        );
        assert!(
            policy
                .judges(&secret)
                .contains(&Subject::Path(".env*".to_owned())),
            "wrapped={wrap}: a path rule did not match the path the call named"
        );
        assert_eq!(
            policy.verdict(&secret),
            Verdict::Deny,
            "wrapped={wrap}: the rule matched and the call was allowed anyway"
        );
    }
}
