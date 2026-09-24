//! The boundary as this program works it out and says it, which is everything up to the spawn.
//!
//! note: the other half of `sandbox.rs`, and it is here because that file is
//! `#![cfg(target_os = "linux")]` and these do not need to be. Nothing in here starts a process:
//! what is under test is which paths `Reach` admits, what a refusal names, what a confinement
//! travels as on a command line, what the scratch directory is allowed to be made through, and
//! which errors `Sandbox::note_for` will claim. All of it is arithmetic over paths, and all of it
//! was running on one platform of the three this crate says it works on - so the `macos-latest`
//! column was green while checking none of it.
//!
//! note: `#![cfg(unix)]` rather than nothing at all. These are written against `/usr`, `/etc` and
//! a `~`, and a root with no drive letter is not an absolute path on Windows - so what they would
//! check there is not what they say they check.

#![cfg(unix)]

use std::{path::PathBuf, sync::Arc};

use kamchatka::{
    sandbox::Sandbox,
    tools::{Careful, Limits, Shell},
};

mod common;

#[test]
fn the_file_tools_are_held_to_the_same_boundary() {
    use kamchatka::sandbox::{Access, Reach};

    let dir = common::workdir("reach").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.clone(),
        extra: vec![PathBuf::from("/usr/share")],
        readable: Vec::new(),
        confined: true,
    };

    // inside, by any spelling. `./` is in here because the first live run of this came back
    // `Not a directory`: an empty remainder joined onto a resolved path appends a separator
    assert_eq!(
        reach.allows("inside.txt", Access::Reading),
        Ok(dir.join("inside.txt"))
    );
    assert_eq!(
        reach.allows("./inside.txt", Access::Reading),
        Ok(dir.join("inside.txt"))
    );
    assert_eq!(reach.allows(".", Access::Reading), Ok(dir.clone()));
    assert!(
        reach
            .allows(dir.join("inside.txt").to_str().unwrap(), Access::Reading)
            .is_ok()
    );
    // ... including one that is not there yet, which is most of what `write` is handed. Compared
    // as strings, deliberately: a `PathBuf` compares by component, so a separator on the end of
    // one is invisible to `assert_eq!` and visible to every `fs` call there is. That is how a
    // `write` which could not create a single file went on passing a test that asserted `Ok`
    let name = |path: &str| {
        reach
            .allows(path, Access::Reading)
            .map(PathBuf::into_os_string)
    };
    assert_eq!(
        name("not-there-yet.txt"),
        Ok(dir.join("not-there-yet.txt").into_os_string()),
        "a file about to be created"
    );
    assert_eq!(
        name("./not-there-yet.txt"),
        Ok(dir.join("not-there-yet.txt").into_os_string())
    );
    assert_eq!(
        name("sub/dir/new.txt"),
        Ok(dir.join("sub/dir/new.txt").into_os_string())
    );
    // ... and the whole of what that is for
    let made = reach
        .allows("not-there-yet.txt", Access::Reading)
        .expect("it is inside");
    std::fs::write(&made, "made").expect("a file about to be created can be created");

    // outside, by every spelling somebody would reach for
    assert!(reach.allows("/etc/passwd", Access::Reading).is_err());
    assert!(
        reach
            .allows("../../../etc/passwd", Access::Reading)
            .is_err(),
        "`..` is resolved, not matched"
    );
    assert!(reach.allows("/etc/../etc/passwd", Access::Reading).is_err());

    // ... including through a symlink, which is why the path is resolved rather than compared
    let link = dir.join("out");
    std::os::unix::fs::symlink("/etc", &link).expect("a symlink");
    assert!(
        reach
            .allows(link.join("passwd").to_str().unwrap(), Access::Reading)
            .is_err(),
        "a symlink out is still out"
    );
    // ... and through one to something that is not there, which cannot be canonicalized and is not
    // a file about to be created either: writing through it creates its target
    let outside = std::env::temp_dir().join(format!("kamchatka-not-there-{}", std::process::id()));
    let dangling = dir.join("dangling");
    std::os::unix::fs::symlink(&outside, &dangling).expect("a symlink");
    assert!(
        reach.allows("dangling", Access::Writing).is_err(),
        "a link to a file that does not exist yet let a write out"
    );
    let relative = dir.join("relative");
    std::os::unix::fs::symlink("../../../../../../../../tmp/kamchatka-nowhere", &relative)
        .expect("a symlink");
    assert!(reach.allows("relative", Access::Writing).is_err());
    let (one, two) = (dir.join("one"), dir.join("two"));
    std::os::unix::fs::symlink(&two, &one).expect("a symlink");
    std::os::unix::fs::symlink(&one, &two).expect("a symlink");
    assert!(
        reach.allows("one", Access::Writing).is_ok(),
        "a loop inside is still inside, and the open is what refuses it"
    );
    // a link inside to something inside that is not there yet is a file about to be created
    std::os::unix::fs::symlink(dir.join("later.txt"), dir.join("soon")).expect("a symlink");
    assert_eq!(
        name("soon"),
        Ok(dir.join("later.txt").into_os_string()),
        "a link inside to a file about to be created"
    );

    // what was opened up on purpose
    assert!(reach.allows("/usr/share/anything", Access::Reading).is_ok());
    assert!(reach.allows("/usr/share/anything", Access::Writing).is_ok());

    // ... and what was opened up for reading and no more. The two answers differ only here, which
    // is why `allows` takes what the tool is about to do rather than defaulting to one of them
    let readable = Reach {
        readable: vec![PathBuf::from("/usr/lib")],
        ..reach.clone()
    };
    assert!(
        readable
            .allows("/usr/lib/anything", Access::Reading)
            .is_ok()
    );
    let refused = readable
        .allows("/usr/lib/anything", Access::Writing)
        .expect_err("a read-only path is not writable");
    assert!(
        refused.contains("reading only"),
        "a model that has just read a file there cannot use \"outside what this session \
         reaches\": {refused}"
    );

    // ... and none of it applies when nobody asked for it
    let open = Reach {
        confined: false,
        ..reach
    };
    assert!(open.allows("/etc/passwd", Access::Reading).is_ok());
}

/// A refusal names everywhere the session reaches, not only the directory it started in.
///
/// note: it used to say "outside {workdir}, which is as far as this session reaches", which was
/// true until somebody passed `--sandbox-allow` and false afterwards - and false in the direction
/// that costs something. A model reads a refusal as the whole boundary, so a path opened up for it
/// on purpose is one it then never goes near, and nothing in front of it says otherwise. The
/// `shell` tool has named them in its own description since the same thing happened to a confined
/// command; these three run in process and were the half left behind.
/// A path checked and then changed into a link out, before it is opened, is refused at the open
/// rather than followed - for reading, and for writing, which would create or empty a file there.
///
/// note: the swap is done by hand between the two calls, which is the whole of what a race is:
/// something else writing to the directory after `allows` has looked at it. Linux only, because
/// `openat2` is what refuses it and elsewhere the open is an ordinary one.
#[cfg(target_os = "linux")]
#[test]
fn a_path_turned_into_a_link_out_after_it_was_checked_is_not_opened() {
    use kamchatka::sandbox::{Access, Reach};

    let base = common::scratch("swapped")
        .canonicalize()
        .expect("it exists");
    let (inside, outside) = (base.join("w"), base.join("elsewhere"));
    std::fs::create_dir_all(inside.join("d")).expect("a directory to check");
    std::fs::create_dir_all(&outside).expect("a directory outside it");
    std::fs::write(inside.join("d").join("notes.txt"), "mine").expect("a file inside");
    std::fs::write(outside.join("notes.txt"), "not yours").expect("a file outside");
    let reach = Reach {
        workdir: inside.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };

    let read = reach
        .allows("d/notes.txt", Access::Reading)
        .expect("it is inside");
    let written = reach
        .allows("d/new.txt", Access::Writing)
        .expect("it is inside");
    std::fs::rename(inside.join("d"), base.join("d-was")).expect("moved aside");
    std::os::unix::fs::symlink(&outside, inside.join("d")).expect("and a link out in its place");

    let refused = reach
        .open(&read, Access::Reading)
        .expect_err("the path leads out now");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    reach
        .open(&written, Access::Writing)
        .expect_err("and so does this one");
    reach
        .open(&read, Access::Writing)
        .expect_err("and writing over one that is there");
    reach
        .replace(&written, b"yours now")
        .expect_err("and replacing, which makes its file in the directory");
    reach
        .replace(&read, b"yours now")
        .expect_err("over one that is there as well");
    assert!(
        !outside.join("new.txt").exists(),
        "nothing was created out there"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("notes.txt")).expect("still there"),
        "not yours",
        "and nothing out there was emptied"
    );

    // and a path that is still what was checked opens as ever
    std::fs::remove_file(inside.join("d")).expect("the link");
    std::fs::rename(base.join("d-was"), inside.join("d")).expect("put back");
    let mut opened = reach
        .open(&read, Access::Reading)
        .expect("it is inside again");
    let mut said = String::new();
    std::io::Read::read_to_string(&mut opened, &mut said).expect("text");
    assert_eq!(said, "mine");
}

#[test]
fn a_refusal_names_what_was_opened_up() {
    use kamchatka::sandbox::{Access, Reach};

    let dir = common::workdir("named").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.clone(),
        extra: vec![PathBuf::from("/usr/share")],
        readable: vec![PathBuf::from("/usr/lib")],
        confined: true,
    };

    let refused = reach
        .allows("/etc/passwd", Access::Reading)
        .expect_err("it is outside");
    assert!(
        refused.contains(&format!("{} read-write", dir.display())),
        "{refused}"
    );
    assert!(refused.contains("/usr/share read-write"), "{refused}");
    assert!(refused.contains("/usr/lib read-only"), "{refused}");

    // and a write refused for being read-only names where it *may* write, which is the same list
    // minus the half that would be a second refusal
    let refused = reach
        .allows("/usr/lib/anything", Access::Writing)
        .expect_err("a read-only path is not writable");
    assert!(refused.contains("/usr/share"), "{refused}");
    assert!(
        !refused.contains("/usr/lib read-only"),
        "it offered the path it had just refused: {refused}"
    );
}

/// `~` is not expanded, and the model is told so rather than left with `No such file or directory`.
///
/// note: the same trap as `access(2)` under Landlock, in a second form. Nothing expands `~` for
/// these tools - they run in process with no shell in front of them - so `~/.gitconfig` joined
/// onto the working directory as a directory literally named `~` and came back absent. A model
/// believes that: it concludes the home directory is empty rather than that its path was taken at
/// its word, and nothing in front of it said otherwise.
#[test]
fn a_leading_tilde_is_refused_in_words_rather_than_expanded() {
    use kamchatka::sandbox::{Access, Reach};

    let dir = common::workdir("tilde").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };

    for path in ["~", "~/.gitconfig", "~/", "~root/.ssh/id_rsa"] {
        let refused = reach
            .allows(path, Access::Reading)
            .expect_err("a leading `~` is not a path this can honour");
        assert!(
            refused.contains("`~` is not expanded here"),
            "it has to say what happened to the path: {refused}"
        );
        assert!(
            refused.contains(&dir.display().to_string()),
            "and where to write one instead: {refused}"
        );
        // note: a refusal that does not close the retry is an invitation to retry. Without this
        // sentence one model sent the same path back six times in a single turn
        assert!(
            refused.contains("refused again"),
            "and that sending the same path back will not help: {refused}"
        );
        // note: and it names no other path. Every concrete path in a refusal is read as a path to
        // try, because a refusal is read under pressure to try something else - two models
        // answered a refusal about `~/notes.txt` by reading the `./~` this used to mention
        assert!(
            !refused.contains("./~"),
            "and offers no second path to reach for: {refused}"
        );
    }

    // the security half: expanding it would have `--no-sandbox` hand over a real home directory on
    // a path the model wrote, so the refusal comes before the unconfined early return
    let open = Reach {
        confined: false,
        ..reach.clone()
    };
    assert!(
        open.allows("~/.ssh/id_rsa", Access::Writing).is_err(),
        "an unconfined session is exactly where this must not expand"
    );

    // and it is the leading `~` only: a vim backup is a real file, and `./~` is how a shell asks
    // for a literal one, so neither is anybody's business but the filesystem's
    assert_eq!(
        reach.allows("notes.txt~", Access::Reading),
        Ok(dir.join("notes.txt~"))
    );
    assert_eq!(reach.allows("./~", Access::Reading), Ok(dir.join("~")));

    // the other half of the pair. A sentence at the point of failure is what lands, but it lands
    // as a surprise unless the argument said so first - which is the arrangement `shell` already
    // has with its confinement, and the reason a description is worth the tokens
    for tool in kamchatka::tools::builtin(
        Shell {
            workdir: dir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
        },
        reach,
        Limits::default(),
    ) {
        let spec = tool.spec();
        let Some(path) = spec.schema["properties"].get("path") else {
            continue;
        };
        let said = path["description"].as_str().unwrap_or_default();
        assert!(
            said.contains("`~` is not expanded"),
            "`{}`'s path argument does not warn about it: {said}",
            spec.id
        );
        // and this is where the literal-`~` spelling lives, because here it is read while
        // choosing rather than while looking for something else to try
        assert!(
            said.contains("`./~`"),
            "`{}`'s path argument does not say how to name one: {said}",
            spec.id
        );
    }
}

/// The scratch directory is never made *through* whatever is already at its name. Its name has to
/// be predictable - it is how the process that spawned the confined one finds it again - and the
/// ruleset grants the resolved path everything a writable root gets, so a link left there by
/// somebody else would have opened up whatever it pointed at and `TMPDIR` would have sent the
/// command into it.
///
/// note: the link here belongs to this test, so it is unlinked and the directory made in its
/// place; what stops one belonging to *somebody else* being unlinked is `/tmp` being sticky, and
/// there is no second account here to write that with. What this checks is the half that can be:
/// whatever the name pointed at is not what the command gets.
#[test]
fn the_scratch_directory_is_never_somebody_elses() {
    use kamchatka::sandbox::make_scratch;

    let root = common::workdir("scratch-name");
    let (elsewhere, path) = (root.join("elsewhere"), root.join("kamchatka-0"));
    std::fs::create_dir_all(&elsewhere).expect("somewhere to point at");
    std::fs::write(elsewhere.join("secret.txt"), "hunter2").expect("something in it");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, &path).expect("a link where the scratch goes");
    #[cfg(not(unix))]
    std::fs::create_dir(&path).expect("a directory where the scratch goes");

    let made = make_scratch(&path).expect("it is this test's own, so it gives way");
    assert_eq!(made, path);
    assert!(
        !std::fs::symlink_metadata(&path)
            .expect("it is there")
            .is_symlink(),
        "the scratch is still the link somebody else planted"
    );
    assert_eq!(
        std::fs::read_dir(&path).expect("a directory").count(),
        0,
        "and the command was handed a directory with things already in it"
    );
    assert!(
        elsewhere.join("secret.txt").exists(),
        "what the link pointed at was opened up rather than left alone"
    );

    // and it is the user's own, for the same reason a written session is: what a command leaves in
    // here is whatever it was working on, and a fresh directory under the default umask is one
    // everyone on the machine can read
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = std::fs::metadata(&path).expect("it is there").permissions();
        assert_eq!(mode.mode() & 0o777, 0o700, "{:o}", mode.mode());
    }

    // a directory left by a run whose process identifier has come round again is the common case,
    // and the command must not inherit its leavings
    std::fs::write(path.join("stale.txt"), "from the last run").expect("a leftover");
    assert_eq!(make_scratch(&path).as_ref(), Some(&path));
    assert_eq!(
        std::fs::read_dir(&path).expect("a directory").count(),
        0,
        "the leftovers of the last run were handed to this one"
    );
}

#[test]
fn what_goes_out_as_arguments_comes_back_as_the_same_sandbox() {
    let sandbox = Sandbox {
        workdir: PathBuf::from("/tmp/work dir"),
        extra: vec![PathBuf::from("/opt/one"), PathBuf::from("/opt/two")],
        readable: vec![PathBuf::from("/opt/three")],
        writable: false,
        network: true,
    };

    let argv = sandbox.argv("echo 'hello world'; ls");
    let (read_back, cmd) = Sandbox::from_argv(&argv).expect("it is one of ours");

    assert_eq!(read_back, sandbox, "a path with a space in it survives");
    assert_eq!(cmd, "echo 'hello world'; ls");
    assert!(Sandbox::from_argv(&[]).is_none());
    assert!(Sandbox::from_argv(&["--help".into()]).is_none());
}

/// A refusal that was the confinement says so, and one that was not says nothing.
#[test]
fn a_permission_error_says_when_the_confinement_caused_it() {
    let confined = Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: false,
    };

    // the shape the live session produced, down to the quotes rustup wraps the path in
    let note = confined
        .note_for(
            "error: could not read settings file: '/home/someone/.rustup/settings.toml': \
             Permission denied (os error 13)\n",
        )
        .expect("the path is out of reach, so this is the confinement");
    assert!(
        note.contains("/home/someone/.rustup/settings.toml"),
        "the useful part is which path: {note}"
    );
    assert!(
        note.contains("/w"),
        "and what it could have used instead: {note}"
    );

    // a path this reaches was refused by its own permissions, and a hedge about the sandbox would
    // send a model looking for a boundary that had nothing to do with it
    assert_eq!(
        confined.note_for("cat: /etc/shadow: Permission denied\n"),
        None
    );

    // nothing refused, nothing to say
    assert_eq!(confined.note_for("ls: no such file\n"), None);

    // refused, and naming no path this can pick out: the general answer rather than silence
    let note = confined
        .note_for("bind: Permission denied\n")
        .expect("something was refused");
    assert!(note.contains("confined"), "{note}");

    // ... and a path that has been opened up is not the confinement either. A real directory,
    // because a root that is not there is not one this opens up - the same answer `Reach::allows`
    // gives, and for the same reason
    let opened_up = common::workdir("opened-up");
    let mut opened = confined.clone();
    opened.readable = vec![opened_up.clone()];
    assert_eq!(
        opened.note_for(&format!(
            "error: could not read settings file: '{}/settings.toml': Permission denied \
             (os error 13)\n",
            opened_up.display()
        )),
        None,
    );
}

/// A path that climbs with `..` out of a directory that is not there is refused, rather than
/// checked as though it were still inside.
///
/// note: `resolve` peels off what does not exist and stops at a `..` it cannot peel, so the path
/// came back with its `..`s in it and the working directory at its front - which a comparison by
/// components passes. On Linux the open that follows refuses it; anywhere that open is an
/// ordinary one, creating the missing directory in between was a way out.
#[test]
fn a_climb_out_of_a_directory_that_is_not_there_is_refused() {
    use kamchatka::sandbox::{Access, Reach};

    let dir = common::scratch("climb").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.join("w"),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };
    std::fs::create_dir_all(dir.join("w")).expect("the working directory");

    for doing in [Access::Reading, Access::Writing] {
        let refused = reach
            .allows("nope/../../../etc/passwd", doing)
            .expect_err("it climbs out through a directory that is not there");
        assert!(refused.contains("`..`"), "{refused}");
    }
    // and a `..` through a directory that is there is resolved as it always was
    std::fs::create_dir_all(dir.join("w").join("here")).expect("a directory");
    assert!(reach.allows("here/../notes.txt", Access::Writing).is_ok());
}

/// A standard error of a great many refused paths is accounted for in a moment, naming three.
///
/// note: every path was compared against every one kept so far, each costing a dozen
/// `canonicalize` calls, before three were kept - minutes of work on a large standard error, after
/// the command had ended, with nothing to interrupt it.
#[test]
fn a_great_many_refusals_are_accounted_for_in_a_moment() {
    use kamchatka::sandbox::Sandbox;

    let confined = Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: false,
    };
    let stderr: String = (0..300_000)
        .map(|nth| format!("/x/{nth}: Permission denied\n"))
        .collect();

    let started = std::time::Instant::now();
    let note = confined.note_for(&stderr).expect("they are out of reach");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(note.contains("/x/0") && note.contains("/x/2"), "{note}");
    assert!(!note.contains("/x/3"), "three are named: {note}");
}
