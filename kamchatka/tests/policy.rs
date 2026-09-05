//! Tests for the permission policy's own rules: what a pattern matches, and what the policy
//! answers about a call because of it.
//!
//! note: these were only ever exercised through the screen, which draws the answer rather than
//! asking for it. A rule that silently fails to match draws nothing and looks exactly like a rule
//! nobody tripped - which is how `.env/` came to be a way of reading `.env` without being asked
//! about it.

use std::path::PathBuf;

use kamchatka::{
    sandbox::Reach,
    tools::{Careful, Subject, path_matches},
};
use nachalnik::{Capability, PermissionId, PermissionRequest, ToolCall, Verdict};
use serde_json::json;

/// What the policy would answer about `read`ing this path.
fn asking_about(policy: &Careful, path: &str) -> Verdict {
    let call = ToolCall::new("c1", "read", json!({ "path": path }));
    let request = PermissionRequest {
        id: PermissionId(1),
        call: call.id.clone(),
        tool: "read".to_owned(),
        capabilities: vec![Capability::Read],
        args: call.args.clone(),
    };

    policy.verdict(&request)
}

/// The rule has to be about the file that will actually be opened, whatever spelling of it the
/// model produced. A pattern is matched against a name and the file is opened at a *resolved*
/// path, and the two used to disagree about the simplest thing there is: a trailing slash.
#[test]
fn a_credential_rule_is_about_the_file_that_gets_opened() {
    let dir = std::env::temp_dir().join(format!("kamchatka-policy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).expect("a temporary directory");
    std::fs::write(dir.join(".env"), "TOKEN=hunter2").expect("something worth protecting");

    let reach = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        confined: true,
    };
    // the moment the credential list exists for: somebody has answered `always` to an ordinary
    // read, so the capability no longer asks and the rule is the only thing left standing
    let policy = Careful::new();
    policy.set(&Subject::Capability(Capability::Read), Verdict::Allow);

    for spelling in [".env", "./.env", ".env/", ".env//", "sub/../.env", ".env/."] {
        assert_eq!(
            reach.allows(spelling).as_deref(),
            Ok(dir.join(".env").as_path()),
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

/// The sandbox is the boundary that does not care about names; these rules do, and say so.
#[test]
fn the_rules_are_about_names_and_a_symlink_is_not_one() {
    let dir = std::env::temp_dir().join(format!("kamchatka-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    std::fs::write(dir.join(".env"), "TOKEN=hunter2").expect("something worth protecting");

    // a name with no rule about it, pointing at a file there is one about
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join(".env"), dir.join("notes.txt")).expect("a symlink");

        assert!(
            !path_matches(".env*", "notes.txt"),
            "a rule is about a name, and this is a different name"
        );
        // it is still inside the working directory, so the reach allows it - which is the honest
        // shape of a name rule and the reason the sandbox is where the boundary is
        let reach = Reach {
            workdir: dir.clone(),
            extra: Vec::new(),
            confined: true,
        };
        assert_eq!(
            reach.allows("notes.txt").as_deref(),
            Ok(dir.join(".env").as_path()),
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Nothing outside the working directory, however it is spelled.
#[test]
fn the_reach_refuses_what_is_outside_it() {
    let dir = std::env::temp_dir().join(format!("kamchatka-reach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).expect("a temporary directory");
    let reach = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        confined: true,
    };

    for outside in ["/etc/passwd", "../outside.txt", "sub/../../outside.txt"] {
        assert!(reach.allows(outside).is_err(), "{outside}");
    }
    // including one that does not exist yet, which is most of what `write` is handed
    assert!(reach.allows("sub/../../new.txt").is_err());
    assert!(reach.allows("sub/new.txt").is_ok());

    // and with the confinement off, nothing is refused
    let open = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        confined: false,
    };
    assert_eq!(open.allows("/etc/passwd"), Ok(PathBuf::from("/etc/passwd")));

    let _ = std::fs::remove_dir_all(&dir);
}
