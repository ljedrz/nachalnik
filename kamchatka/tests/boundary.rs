//! The boundary as this program works it out and says it, which is everything up to the spawn.
//!
//! note: the other half of `sandbox.rs`, and a file of its own because nothing in here needs the
//! kernel to confine anything. That file skips where Landlock does not hold; these run wherever
//! the suite does. What is under test is which paths `Reach` admits, what a refusal names, what a
//! confinement travels as on a command line, what the scratch directory is allowed to be made
//! through, and which errors `Sandbox::note_for` will claim - all of it arithmetic over paths.

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

/// A working directory that may be searched but not listed is reached all the same, for reading
/// and for writing.
///
/// note: the open starts from a descriptor for the directory the path was allowed under, and the
/// ordinary way to get one wants read permission on it. `O_PATH` asks for none, which is what a
/// home somebody else serves, or a checkout shared with a group, is set up to need. Writing is the
/// harder half: the new file a `write` makes is made through that same descriptor, so a directory
/// that can be searched and written in but not listed would be reached for a read and not for a
/// write - and `replace` is the one that hands the descriptor to the temporary it renames over the
/// old file, so it is the one that has to be tried.
#[test]
fn a_directory_that_may_only_be_searched_is_still_reached() {
    use kamchatka::sandbox::{Access, Reach};
    use std::os::unix::fs::PermissionsExt;

    let dir = common::workdir("searchable")
        .canonicalize()
        .expect("it exists");
    let reach = Reach {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };
    let read = reach
        .allows("inside.txt", Access::Reading)
        .expect("it is inside");
    let written = reach
        .allows("new.txt", Access::Writing)
        .expect("it is inside");

    let set = |mode| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode));
    // search and write, but no read: a directory something else creates files in and nobody lists
    set(0o333).expect("write and search only");
    let listed = std::fs::read_dir(&dir).is_ok();
    let opened = reach.open(&read, Access::Reading).map(|mut opened| {
        let mut said = String::new();
        std::io::Read::read_to_string(&mut opened, &mut said).map(|_| said)
    });
    let replaced = reach.replace(&written, b"made in a directory that is not listed");
    set(0o755).expect("put back");

    if listed {
        eprintln!("skipped: the owner of a directory can list it here without read");
        return;
    }
    let said = opened
        .expect("a file in a directory that may only be searched")
        .expect("text");
    assert_eq!(said, "hello");

    replaced.expect("and a new file is made in a directory that may only be searched");
    assert_eq!(
        std::fs::read_to_string(dir.join("new.txt")).expect("it was made"),
        "made in a directory that is not listed",
    );
    // sorted, because a listing comes in whatever order the filesystem keeps its entries in
    let mut left: Vec<_> = std::fs::read_dir(&dir)
        .expect("the directory, once it can be listed again")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["inside.txt", "new.txt"],
        "and nothing left beside it"
    );
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
/// something else writing to the directory after `allows` has looked at it. `openat2` is what
/// refuses it, so on a kernel without one - older than 5.6, or behind a container filter that
/// refuses the call - the open is an ordinary one and this fails, which is the right way for that
/// kernel to be found out.
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

    // and a write to a path that is in no root at all is refused as outside the reach rather
    // than as one of the read-only ones, which would name a path it is not under
    let refused = reach
        .allows("/nowhere/in/particular", Access::Writing)
        .expect_err("it is outside everything");
    assert!(
        refused.contains("outside what this session reaches"),
        "a path in no root is not read-only, it is out of reach: {refused}"
    );
    assert!(
        !refused.contains("reading only"),
        "it named a read-only root the path is not under: {refused}"
    );
}

/// Under a refused `fs:write` the refusal calls the reach read-only, in the words the confinement
/// uses for the same session.
///
/// note: through `tools::builtin`, because the policy reaches the refusal through the tool. `Reach`
/// alone knows nothing of it, and called the working directory read-write while `shell` in the
/// same session called it read-only.
#[tokio::test]
async fn a_refusal_under_a_refused_write_says_read_only() {
    use kamchatka::{sandbox::Reach, tools::Subject};
    use nachalnik::{Capability, OutputSink, Verdict, test::call};

    let dir = common::workdir("named-read-only")
        .canonicalize()
        .expect("it exists");
    let opened = PathBuf::from("/usr/share");
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::Capability(Capability::fs("write")), Verdict::Deny);
    let tools = kamchatka::tools::builtin(
        Shell {
            workdir: dir.clone(),
            extra: vec![opened.clone()],
            readable: Vec::new(),
            devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
            policy: policy.clone(),
            confiner: None,
            limits: Limits::default(),
            stragglers: Default::default(),
        },
        Reach {
            workdir: dir.clone(),
            extra: vec![opened.clone()],
            readable: Vec::new(),
            confined: true,
        },
        Limits::default(),
    );
    let fs = tools
        .iter()
        .find(|it| it.spec().id == "fs")
        .expect("`fs` is one of them");

    let refused = fs
        .invoke(
            &call(
                "c1",
                "fs",
                serde_json::json!({ "action": "read", "path": "/etc/passwd" }),
            ),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answers")
        .content
        .to_text()
        .into_owned();
    let confined = Sandbox::of(&policy, dir.clone(), vec![opened], Vec::new(), false).to_string();
    for said in [
        format!("{} read-only", dir.display()),
        "/usr/share read-only".to_owned(),
    ] {
        assert!(confined.contains(&said), "{confined}");
        assert!(refused.contains(&said), "{refused}");
    }
    assert!(!refused.contains("read-write"), "{refused}");
}

/// A system file refused by `fs` is refused as one `shell` reads, and where `shell` is refused as
/// well, as any other path outside the reach.
///
/// note: the system directories are in the shell's reach and not in this one, so a refusal that
/// sent the model to ask for `/etc/passwd` to be opened up had it asking for a file it could
/// already `cat`.
#[tokio::test]
async fn a_system_file_is_refused_by_fs_as_one_shell_reads() {
    use kamchatka::{sandbox::Reach, tools::Subject};
    use nachalnik::{Capability, OutputSink, Verdict, test::call};

    let dir = common::workdir("named-system")
        .canonicalize()
        .expect("it exists");
    let refused = |policy: Arc<Careful>, path: &'static str| {
        let tools = kamchatka::tools::builtin(
            Shell {
                workdir: dir.clone(),
                extra: Vec::new(),
                readable: Vec::new(),
                devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
                policy,
                confiner: None,
                limits: Limits::default(),
                stragglers: Default::default(),
            },
            Reach {
                workdir: dir.clone(),
                extra: Vec::new(),
                readable: Vec::new(),
                confined: true,
            },
            Limits::default(),
        );
        async move {
            let fs = tools
                .iter()
                .find(|it| it.spec().id == "fs")
                .expect("`fs` is one of them");
            fs.invoke(
                &call(
                    "c1",
                    "fs",
                    serde_json::json!({ "action": "read", "path": path }),
                ),
                OutputSink::disconnected(),
            )
            .await
            .expect("the tool answers")
            .content
            .to_text()
            .into_owned()
        }
    };

    let system = refused(Arc::new(Careful::new()), "/etc/passwd").await;
    assert!(system.contains("`shell` reads"), "{system}");
    assert!(system.contains(&dir.display().to_string()), "{system}");
    assert!(!system.contains("opened up"), "{system}");

    let elsewhere = refused(Arc::new(Careful::new()), "/nowhere/in/particular").await;
    assert!(elsewhere.contains("opened up"), "{elsewhere}");

    let policy = Arc::new(Careful::new());
    policy.set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);
    let unrun = refused(policy, "/etc/passwd").await;
    assert!(!unrun.contains("`shell`"), "{unrun}");
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
        // note: and the shell it names is `fs`'s, for the reason `PATH_ARG` gives: "no shell in
        // front of these tools" reads as a fact about the session beside a `shell` tool
        assert!(
            refused.contains("`fs` does not go through a shell") && !refused.contains("no shell"),
            "and says it is `fs` that has no shell, not the session: {refused}"
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
    // as a surprise unless the tool said so first - which is the arrangement `shell` already has
    // with its confinement, and the reason a description is worth the tokens. Said once for every
    // `path` in `fs`'s own description, since each operation's arguments sit under `call`
    let fs = kamchatka::tools::builtin(
        Shell {
            workdir: dir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
            stragglers: Default::default(),
        },
        reach,
        Limits::default(),
    )
    .into_iter()
    .map(|tool| tool.spec())
    .find(|spec| spec.id == "fs")
    .expect("`fs` is built in");
    let said = &fs.description;
    assert!(
        said.contains("Every `path` is") && said.contains("`~` is not expanded"),
        "`fs` does not warn about it: {said}"
    );
    // and this is where the literal-`~` spelling lives, because here it is read while choosing
    // rather than while looking for something else to try
    assert!(
        said.contains("`./~`"),
        "`fs` does not say how to name one: {said}"
    );
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

    std::os::unix::fs::symlink(&elsewhere, &path).expect("a link where the scratch goes");

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

/// Where the gate holds, the network a command is handed follows the stance and what a person
/// allowed for the call: open for an `allow` or a granted call, shut for a `deny`, and asked about
/// otherwise.
///
/// note: `tests/sandbox.rs` runs commands under each of these, but through confinements it builds
/// by hand; this is where the policy becomes one.
#[test]
fn the_network_a_command_is_handed_follows_the_stance_and_the_grant() {
    use kamchatka::{sandbox::Network, tools::Subject};
    use nachalnik::{Capability, Verdict};

    let dir = common::workdir("network-of");
    let policy = Arc::new(Careful::new());
    policy.gate_the_network();
    let network = |granted| Sandbox::of(&policy, dir.clone(), Vec::new(), Vec::new(), granted);
    let stance = |verdict| policy.set(&Subject::Capability(Capability::net("reach")), verdict);

    assert_eq!(network(false).network, Network::Asked);
    assert_eq!(network(true).network, Network::Open);
    stance(Verdict::Allow);
    assert_eq!(network(false).network, Network::Open);
    stance(Verdict::Deny);
    assert_eq!(network(false).network, Network::Shut);
}

/// A port a session is served on is closed to the commands confined while it is, and opened again
/// once the session stops listening.
#[tokio::test]
async fn a_served_port_is_closed_to_commands_while_it_is_served() {
    let dir = common::workdir("served-port-closed");
    let closed = || Sandbox::of(&Careful::new(), dir.clone(), Vec::new(), Vec::new(), true).closed;

    let server = kamchatka::remote::Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("it listens");
    let port: u16 = server
        .address()
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .expect("a port");
    assert!(closed().contains(&port), "{:?}", closed());

    drop(server);
    assert!(!closed().contains(&port), "{:?}", closed());
}

#[test]
fn what_goes_out_as_arguments_comes_back_as_the_same_sandbox() {
    use kamchatka::sandbox::Network;

    // every way the network can be, because each is a word on the command line and a word read
    // back as another is a command confined the wrong way
    for network in [Network::Open, Network::NoTcp, Network::Shut, Network::Asked] {
        let sandbox = Sandbox {
            workdir: PathBuf::from("/tmp/work dir"),
            extra: vec![PathBuf::from("/opt/one"), PathBuf::from("/opt/two")],
            readable: vec![PathBuf::from("/opt/three")],
            writable: false,
            network,
            devices: vec![PathBuf::from("/dev/null"), PathBuf::from("/dev/pts")],
            closed: vec![7878, 9],
        };

        let argv = sandbox.argv("echo 'hello world'; ls");
        let (read_back, cmd) = Sandbox::from_argv(&argv).expect("it is one of ours");

        assert_eq!(read_back, sandbox, "a path with a space in it survives");
        assert_eq!(cmd, "echo 'hello world'; ls");

        // and the same without the session's word, as somebody running it by hand writes it
        let by_hand: Vec<_> = argv
            .iter()
            .filter(|word| *word != kamchatka::sandbox::SESSION_FLAG)
            .cloned()
            .collect();
        assert_eq!(
            by_hand.len() + 1,
            argv.len(),
            "the word was there to take out"
        );
        assert_eq!(Sandbox::from_argv(&by_hand), Some((sandbox, cmd)));
    }
    assert!(Sandbox::from_argv(&[]).is_none());
    assert!(Sandbox::from_argv(&["--help".into()]).is_none());
}

/// A refusal that was the confinement says so, and one that was not says nothing.
#[test]
fn a_permission_error_says_when_the_confinement_caused_it() {
    use std::os::unix::fs::PermissionsExt as _;

    let confined = Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
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
    // gives, and for the same reason - and a real file its own permissions refuse, since a file
    // in reach that could be read would have been
    let opened_up = common::workdir("opened-up");
    let settings = opened_up.join("settings.toml");
    std::fs::write(&settings, "").expect("a file");
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o000))
        .expect("its permissions");
    let mut opened = confined.clone();
    opened.readable = vec![opened_up.clone()];
    assert_eq!(
        opened.note_for(&format!(
            "error: could not read settings file: '{}': Permission denied (os error 13)\n",
            settings.display()
        )),
        None,
    );

    // but a write there that the person could have made is the confinement: reached for reading
    // and not for writing, as `--sandbox-read` is and as the working directory is under
    // `--deny fs:write`
    let note = opened
        .note_for(&format!(
            "sh: line 1: {}/out.txt: Permission denied\n",
            opened_up.display()
        ))
        .expect("a write the person could make, refused where this reads only");
    assert!(note.contains("may read and not write"), "{note}");
}

/// Each of the three spellings of a refusal is a refusal on its own.
///
/// note: they are listed together in `refused` and joined by `||` because any one of them is the
/// same refusal written by a different layer - a C program's `strerror`, Rust's `io::Error`
/// display, and the errno name itself. A message carrying the errno alone is written by programs
/// that have it and nothing else to say: `EACCES` out of a Go client, `os error 13` out of
/// something that formats the raw result. Read as a conjunction, both go unmentioned, and the
/// session carries on as though nothing had been refused.
#[test]
fn a_refusal_written_in_any_of_its_spellings_is_a_refusal() {
    use kamchatka::sandbox::Sandbox;

    let confined = Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
    };

    // a refusal naming nothing is owed the general sentence, and a refusal naming a path out of
    // reach is owed that path - both only if the line was recognised as a refusal at all
    for (stderr, said) in [
        (
            "sh: line 1: /home/someone/.ssh/id_rsa: os error 13\n",
            "/home/someone/.ssh/id_rsa is outside what this session reaches",
        ),
        (
            "sh: line 1: /home/someone/.ssh/id_rsa: EACCES\n",
            "/home/someone/.ssh/id_rsa is outside what this session reaches",
        ),
        // the same refusal with nothing in it to pick a path out of
        ("connect: EACCES (13)\n", "ran confined"),
    ] {
        let note = confined
            .note_for(stderr)
            .unwrap_or_else(|| panic!("`{stderr}` is a refusal and nothing was said of it"));
        assert!(note.contains(said), "{stderr}: {note}");
    }
}

/// A token that begins with a slash is a path, and is named whatever else is in it.
///
/// note: the filter leaves a URL out by asking more of a relative token than a `/` in it, and the
/// question is which clause a token reaches first. A token written `/tmp/odd://etc/passwd` is a
/// command quoting its own path with the empty component left in - and it reaches the slash at the
/// front, which is a path, so it is named. Under the second clause alone it would be a URL and
/// dropped, and the refusal would say only that something had been refused.
///
/// note: the token is the shape and not a path anybody has: what is checked is the order the two
/// clauses are read in, and this is the only token where they disagree.
#[test]
fn an_absolute_path_that_looks_like_a_url_is_still_named() {
    use kamchatka::sandbox::Sandbox;

    let confined = Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
    };

    let note = confined
        .note_for("sh: /tmp/odd://etc/passwd: Permission denied\n")
        .expect("something was refused, and it named the path");
    assert!(note.contains("/tmp/odd://etc/passwd"), "{note}");

    // ... and a URL that is not a path is left out, which is what the second clause is for: a
    // `docker` refusal naming a registry does not send a model after a path called `https:`
    let note = confined
        .note_for("dial tcp: lookup https://registry.example/v2/: Permission denied\n")
        .expect("something was refused");
    assert!(
        !note.contains("registry.example"),
        "a URL is not a path this session reaches or does not: {note}"
    );
}

/// What the startup probe reports as gated is what the child said, not what this kernel could do.
///
/// note: `gated` is the answer a session's whole network stance is built on: `Setup::wire` hands
/// `shell` a confiner only where the probe held, and the permissions tab draws the same probe. A
/// child that did not put the gate on says so, and a child that did not get a ruleset says so
/// either - and in neither case is the network behind anything whatever, whatever `holds()` says
/// about this kernel. Reading either half on its own would answer yes to a child that reported no.
///
/// note: asked of a stand-in for the child rather than of the real binary, because the two halves
/// cannot both be true of it on one machine: a kernel that confines is a kernel whose ruleset took,
/// and the case that matters is the report that says otherwise. The stand-in is handed the same
/// arguments and answers with the line the confined child writes.
#[test]
fn the_gate_is_reported_only_where_the_child_said_it_went_on() {
    let stand_in = common::scratch("stand-in").join("probe.sh");
    for (report, said) in [
        // the ruleset took and the gate did not go on, which is what a child whose standard input
        // is not the socket comes back with
        ("full", false),
        // the ruleset did not take, whatever the gate said about itself
        ("unavailable gated", false),
    ] {
        std::fs::write(
            &stand_in,
            format!("#!/bin/sh\necho 'kamchatka-confinement:{report}' >&2\n"),
        )
        .expect("the stand-in");
        std::fs::set_permissions(
            &stand_in,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("it is run");

        let probed = kamchatka::sandbox::available(&stand_in);
        assert!(
            probed.gated == said,
            "the child reported `{report}` and this said gated: {}",
            probed.gated
        );
    }
}

/// A relative path in a refusal is judged where the command ran, and is never read as an absolute
/// one: a script with no execute bit is its own permissions, and a write one directory up is the
/// boundary, named as the command wrote it.
#[test]
fn a_relative_path_is_judged_from_the_working_directory() {
    let confined = Sandbox {
        workdir: common::workdir("relative-refusal"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
    };

    assert_eq!(
        confined.note_for("sh: line 1: ./build.sh: Permission denied\n"),
        None,
        "a file in the working directory refused is its own permissions"
    );
    let note = confined
        .note_for("sh: line 1: ../escape.txt: Permission denied\n")
        .expect("one directory up is out of reach");
    assert!(note.starts_with("[../escape.txt is outside"), "{note}");
}

/// `/dev` is accounted for as it is granted: a device there was reached, so a refusal of it is its
/// own permissions and nothing is said, while a listing of `/dev` or a file made in it is the
/// boundary and is named.
#[test]
fn a_refusal_in_dev_is_accounted_for_as_dev_is_granted() {
    let confined = Sandbox {
        workdir: common::workdir("dev-accounts"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
    };

    assert_eq!(
        confined.note_for("sh: /dev/null: Permission denied\n"),
        None,
        "a device that is there is one the command reached"
    );
    let listed = confined
        .note_for("ls: cannot open directory '/dev': Permission denied\n")
        .expect("a listing of /dev is the boundary");
    assert!(listed.contains("/dev is outside"), "{listed}");
    let made = confined
        .note_for("touch: cannot touch '/dev/made-up-by-a-test': Permission denied\n")
        .expect("a file made in /dev is the boundary");
    assert!(made.contains("/dev/made-up-by-a-test"), "{made}");
}

/// What the kernel says about a scope, asked out of the crate rather than out of the program.
///
/// note: the crate's own spelling deliberately - a hard requirement, and a ruleset that is only
/// built, which restricts no process. `landlock` is a dependency of the crate rather than of its
/// tests, and the question has to be put to the kernel rather than read off a version number for
/// either of these to mean anything.
fn the_kernel_has(scope: landlock::Scope) -> bool {
    use landlock::{CompatLevel, Compatible, Ruleset, RulesetAttr};

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .scope(scope)
        .is_ok()
}

/// What this program says about a Landlock scope is what the kernel says about it.
///
/// note: neither answer is skipped and neither is loose. A `false` where the kernel has the scope
/// leaves a confined command free to reach an abstract socket made outside it: the X server's,
/// which takes a connection from any process of the user's and can type into the person's
/// terminal. A `true` where it does not has `scope()` come back `Err` over a scope the kernel
/// never heard of, which is a `Confinement::Unavailable` and every command refused with nothing
/// run. One is a hole and the other is a program that does nothing, and on a kernel that confines
/// at all neither is a failure the suite would see.
#[test]
fn what_this_program_says_about_a_scope_is_what_the_kernel_says() {
    for (claimed, scope, what) in [
        (
            kamchatka::sandbox::confines_abstract_sockets as fn() -> bool,
            landlock::Scope::AbstractUnixSocket,
            "an abstract socket",
        ),
        (
            kamchatka::sandbox::confines_signals as fn() -> bool,
            landlock::Scope::Signal,
            "a signal",
        ),
    ] {
        let (said, kernel) = (claimed(), the_kernel_has(scope));
        assert_eq!(
            said, kernel,
            "{what} is scoped here or nowhere, and this program says {said} where the kernel says \
             {kernel}"
        );
    }
}

/// A refused connection to a socket outside what the session may write is the confinement where the
/// kernel governs sockets, and the socket's own permissions where it does not; one the session may
/// write is never the confinement.
///
/// note: the socket's path is readable either way, which is why the rule for files said nothing:
/// a socket is held to the writing half. This asks the same kernel `tests/sandbox.rs` confines
/// against, and answers for both kinds.
#[test]
fn a_refused_socket_is_the_confinement_where_the_kernel_says_it_is() {
    use std::os::unix::net::UnixListener;

    let workdir = common::workdir("socket-accounts");
    let inside = workdir.join("s.sock");
    let outside = common::scratch("socket-accounts-out").join("s.sock");
    let _held = (
        UnixListener::bind(&inside).expect("a socket inside"),
        UnixListener::bind(&outside).expect("a socket outside"),
    );
    let confined = Sandbox {
        workdir,
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
    };
    let refused = |socket: &std::path::Path| {
        confined.note_for(&format!(
            "dial unix {}: connect: permission denied\n",
            socket.display()
        ))
    };

    assert_eq!(
        refused(&inside),
        None,
        "one it may write is its own permissions"
    );
    match kamchatka::sandbox::confines_unix_sockets() {
        true => {
            let note = refused(&outside).expect("the kernel refused it, not the socket");
            assert!(note.contains("is a socket outside"), "{note}");
        }
        false => assert_eq!(
            refused(&outside),
            None,
            "the kernel does not govern sockets"
        ),
    }
}

/// A refusal naming a path that climbs out of the working directory past a directory that is not
/// there is the boundary, as `Reach::allows` has it, rather than a path inside.
#[test]
fn a_refusal_that_climbs_out_past_a_missing_directory_is_the_boundary() {
    let workdir = common::workdir("climb-accounts")
        .canonicalize()
        .expect("it exists");
    let confined = Sandbox {
        workdir: workdir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
    };

    let climbing = workdir.join("missing/../../../../kamchatka-nowhere/secret");
    let note = confined
        .note_for(&format!("cat: {}: Permission denied\n", climbing.display()))
        .expect("it climbed out, so the refusal is the confinement");
    assert!(note.contains("is outside"), "{note}");
}

/// A path that climbs with `..` out of a directory that is not there is refused, rather than
/// checked as though it were still inside.
///
/// note: `resolve` peels off what does not exist and stops at a `..` it cannot peel, so the path
/// came back with its `..`s in it and the working directory at its front - which a comparison by
/// components passes. Where there is `openat2` the open that follows refuses it; on a kernel
/// without one, where that open is an ordinary one, creating the missing directory in between was
/// a way out.
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

/// A chain of links longer than the bound is refused rather than followed to its end.
///
/// note: `LINKS` is there because two links can point at each other and forty of them can be
/// laid out by whoever made them. A counter that never moved is a bound that is not a bound: the
/// whole chain is followed, and where it lands is then compared with a reach it walked out of -
/// so the refusal that must be given is the one that says where it leads cannot be checked.
///
/// note: the chain ends at a file that is there and out of reach, so the last link is followed to
/// the end of the walk rather than to somewhere that does not exist. A chain within the bound is
/// followed here as well, which is what says the refusal is about the length.
#[test]
fn a_chain_of_links_longer_than_the_bound_is_refused() {
    use kamchatka::sandbox::{Access, Reach};

    let dir = common::scratch("links").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.join("w"),
        extra: Vec::new(),
        readable: Vec::new(),
        confined: true,
    };
    std::fs::create_dir_all(&reach.workdir).expect("the working directory");

    let outside = dir.join("secret");
    std::fs::write(&outside, "").expect("a file out of reach");
    // note: longer than twice what the kernel will resolve in one go, because `resolve` compares
    // a prefix of the chain at a time and `canonicalize` answers for a chain of forty links. A
    // counter over a chain this length is the only thing standing between the walk and the end
    let chain = 82;
    for nth in 0..chain {
        let target = match nth + 1 == chain {
            true => outside.clone(),
            false => PathBuf::from(format!("l{}", nth + 1)),
        };
        std::os::unix::fs::symlink(&target, reach.workdir.join(format!("l{nth}"))).expect("a link");
    }

    let refused = reach
        .allows("l0", Access::Reading)
        .expect_err("a chain this long is not followed to its end, so where it leads is unknown");
    assert!(refused.contains("too long to follow"), "{refused}");

    // ... and one within the bound is followed, so the refusal is about the length of the chain
    // and not about links in general
    std::os::unix::fs::symlink(&outside, reach.workdir.join("near")).expect("a link");
    let refused = reach
        .allows("near", Access::Reading)
        .expect_err("it leads out of the working directory");
    assert!(
        refused.contains("outside what this session reaches"),
        "{refused}"
    );
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
        network: kamchatka::sandbox::Network::NoTcp,
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        closed: Vec::new(),
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

/// What a confined command is said to reach names the ports closed to it, where TCP is otherwise
/// open, and says nothing of them where TCP is refused anyway.
///
/// note: found live. A served session's shell was refused a connection to the session's own port,
/// and the note on the refusal said the network was reachable - which left the one refusal that
/// happened unexplained.
#[test]
fn a_closed_port_is_part_of_what_a_command_is_said_to_reach() {
    let served = |network| Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network,
        devices: Vec::new(),
        closed: vec![18790],
    };

    let open = served(kamchatka::sandbox::Network::Open).to_string();
    assert!(
        open.ends_with("but for port 18790 on this machine, where this session is served"),
        "{open}"
    );
    let shut = served(kamchatka::sandbox::Network::NoTcp).to_string();
    assert!(!shut.contains("18790"), "{shut}");
}
