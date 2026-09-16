//! `grep` and `glob`, against real files.
//!
//! note: through `tools::builtin` rather than by constructing either of them, because they are not
//! public types and because what is being checked includes the wiring: a session's path rules are
//! the *shell's* policy handle, and a search that consulted a policy of its own would pass every
//! test in here while honouring nothing anybody answered.
//!
//! note: real files rather than a fixture in memory. The whole argument for these two tools is
//! that ripgrep's walker knows what a `.gitignore` means and a hand-rolled one does not, and
//! nothing but a directory on a disk can check that.

mod common;

use std::{path::Path, sync::Arc};

use common::scratch;
use kamchatka::{
    sandbox::Reach,
    tools::{Careful, Limits, Shell},
};
use nachalnik::{OutputSink, Tool, ToolCall, test::call};
use serde_json::{Value, json};

/// The tools as a session gets them, held to `dir`.
fn tools(dir: &Path) -> Vec<Arc<dyn Tool>> {
    kamchatka::tools::builtin(
        Shell {
            workdir: dir.to_path_buf(),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: Arc::new(Careful::new()),
            confiner: None,
            limits: Limits::default(),
        },
        Reach {
            workdir: dir.to_path_buf(),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: true,
        },
        Limits::default(),
    )
}

/// Calls one of them and hands back what the model would read.
async fn ask(dir: &Path, tool: &str, args: Value) -> String {
    let tools = tools(dir);
    let found = tools
        .iter()
        .find(|it| it.spec().id == tool)
        .unwrap_or_else(|| panic!("`{tool}` should be one of the built-in tools"));

    let call: ToolCall = call("c1", tool, args);
    found
        .invoke(&call, OutputSink::disconnected())
        .await
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned()
}

/// Writes a file, making the directories above it.
fn put(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a directory to write in");
    }
    std::fs::write(path, text).expect("a file to be written");
}

/// A small tree with something to find in it.
fn tree(name: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    put(
        &dir,
        "src/kernel.rs",
        "pub struct Kernel;\nimpl Kernel {}\n",
    );
    put(
        &dir,
        "src/app/keys.rs",
        "// the Kernel is next door\nfn go() {}\n",
    );
    put(&dir, "notes.md", "nothing to see\n");

    dir
}

#[tokio::test]
async fn grep_answers_with_the_path_the_line_and_the_number() {
    let dir = tree("grep-answers");
    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;

    assert!(
        said.starts_with("3 match(es) in 2 file(s) · 3 file(s) searched"),
        "the count comes first, above the lines: {said}"
    );
    // relative to the working directory, which is the spelling `read` takes back
    assert!(
        said.contains("src/app/keys.rs:1:// the Kernel is next door"),
        "{said}"
    );
    assert!(
        said.contains("src/kernel.rs:1:pub struct Kernel;"),
        "{said}"
    );
    assert!(said.contains("src/kernel.rs:2:impl Kernel {}"), "{said}");
    assert!(
        !said.contains(&dir.display().to_string()),
        "no absolute paths: {said}"
    );
}

/// An empty answer says which empty it is: nothing there, or nothing searched.
///
/// note: the same distinction the context pane draws between a filter that matched nothing and a
/// pane with nothing in it. A model told only "no matches" cannot tell a pattern that is wrong
/// from a path that is, and both of its next moves are guesses.
#[tokio::test]
async fn nothing_found_says_how_much_was_looked_at() {
    let dir = tree("grep-nothing");
    let said = ask(&dir, "grep", json!({ "pattern": "Compactor" })).await;

    assert_eq!(said, "no matches for `Compactor` in . · 3 file(s) searched");

    // and a path that holds nothing to search says so with the same sentence, a different number
    let empty = ask(
        &dir,
        "grep",
        json!({ "pattern": "Kernel", "glob": "*.toml" }),
    )
    .await;
    assert!(empty.ends_with("0 file(s) searched"), "{empty}");
}

/// What the walker skips, and what it does not.
#[tokio::test]
async fn the_walk_knows_what_a_gitignore_means() {
    let dir = tree("grep-ignored");
    put(&dir, ".gitignore", "build/\n");
    put(&dir, "build/generated.rs", "pub struct Kernel;\n");
    put(
        &dir,
        ".github/workflows/ci.yml",
        "# the Kernel is built here\n",
    );
    put(&dir, ".git/config", "Kernel\n");

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;

    assert!(
        !said.contains("build/generated.rs"),
        "a `.gitignore`d file is not searched: {said}"
    );
    assert!(
        !said.contains(".git/config"),
        "`.git` is a database, not source: {said}"
    );
    // hidden *is* searched, deliberately: a model that cannot find `.github/workflows` concludes
    // the file is not there
    assert!(
        said.contains(".github/workflows/ci.yml:1:"),
        "a hidden file is searched: {said}"
    );
}

/// A path rule bars a file from a walk, and the answer says how many it barred.
///
/// note: the hole this closes is that `Careful` matches path rules against the path *in the call*,
/// and a search names a directory. Without this, `read .env` is a question and `grep -r .` reads
/// the same file without one - which would make the rule a decoration.
#[tokio::test]
async fn a_path_rule_keeps_a_walk_out_of_a_file() {
    let dir = tree("grep-barred");
    put(&dir, ".env", "TOKEN=Kernel-of-a-secret\n");

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    assert!(
        !said.contains("TOKEN="),
        "`.env*` is `ask` by default, and a walk cannot ask: {said}"
    );
    assert!(
        said.contains("skipped: 1 file(s) a path rule says to ask about"),
        "and it says it did: {said}"
    );

    // named outright it is searched, because *that* path is one the policy is asked about, the
    // same way `read` of it is - the rule is about what a walk wanders into
    let named = ask(&dir, "grep", json!({ "pattern": "Kernel", "path": ".env" })).await;
    assert!(named.contains(".env:1:TOKEN=Kernel-of-a-secret"), "{named}");
}

/// A link is a question about the reach, not about links.
///
/// note: both halves, because the first rule here was "skip every link" and a live run in this
/// repository argued it down: five crates carry a `LICENSE-MIT` link to the file at the root, so
/// every answer to every search led with `skipped: 5 symbolic link(s)` - a line claiming something
/// was withheld when nothing was. What decides is `Reach::allows`, which is the same call, with
/// the same answer, that `read` makes about the same path.
#[cfg(unix)]
#[tokio::test]
async fn a_link_is_read_where_it_points_inside_and_counted_where_it_points_out() {
    let dir = tree("grep-links");
    let outside = scratch("grep-links-outside");
    put(&outside, "secret.rs", "pub struct Kernel;\n");
    std::os::unix::fs::symlink(outside.join("secret.rs"), dir.join("out.rs"))
        .expect("a link to be made");
    std::os::unix::fs::symlink(dir.join("src/kernel.rs"), dir.join("in.rs"))
        .expect("a link to be made");

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    assert!(
        !said.contains("out.rs"),
        "a link out of the working directory is not a way into it: {said}"
    );
    assert!(
        said.contains("skipped: 1 link(s) pointing out of reach"),
        "{said}"
    );
    assert!(
        said.contains("in.rs:1:pub struct Kernel;"),
        "and one inside it is an ordinary file with a second name: {said}"
    );
}

/// A binary file is skipped whole rather than answered with a line of object code.
#[tokio::test]
async fn a_binary_file_is_counted_rather_than_quoted() {
    let dir = tree("grep-binary");
    std::fs::write(dir.join("a.out"), b"Kernel\x00\x01\x02Kernel").expect("a file to be written");

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    assert!(!said.contains("a.out"), "{said}");
    assert!(said.contains("skipped: 1 binary file(s)"), "{said}");
}

/// The answer stops at a number of matches, and says that it stopped.
///
/// note: the cut that matters. A byte limit takes the tail of the last file and leaves the model
/// believing it has seen the rest; this says what it did in a sentence the model can act on.
#[tokio::test]
async fn too_many_matches_stop_and_say_they_stopped() {
    let dir = scratch("grep-many");
    let many: String = (0..150).map(|n| format!("let x{n} = Kernel;\n")).collect();
    put(&dir, "many.rs", &many);

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    assert!(
        said.starts_with("100 match(es) in 1 file(s) · that is as many as this answers with"),
        "{said}"
    );
    assert!(said.contains("there may be more"), "{said}");
    assert_eq!(
        said.lines().skip(1).count(),
        100,
        "a hundred lines under the one that says so"
    );
}

/// Context lines come back marked as context.
#[tokio::test]
async fn context_lines_are_marked_as_context() {
    let dir = scratch("grep-context");
    put(&dir, "a.rs", "one\ntwo\nKernel\nfour\nfive\n");

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel", "context": 1 })).await;
    assert!(said.contains("a.rs-2-two"), "before: {said}");
    assert!(said.contains("a.rs:3:Kernel"), "the match: {said}");
    assert!(said.contains("a.rs-4-four"), "after: {said}");
    assert!(
        said.starts_with("1 match(es) in 1 file(s)"),
        "context is not counted as a match: {said}"
    );
}

/// A pattern that does not parse costs no walk, and says what to do about it.
#[tokio::test]
async fn a_pattern_that_is_not_a_regex_says_what_to_escape() {
    let dir = tree("grep-bad-regex");
    let said = ask(&dir, "grep", json!({ "pattern": "Vec<u8>(" })).await;

    assert!(
        said.contains("is not a regular expression this understands"),
        "{said}"
    );
    assert!(
        said.contains("Escape anything you meant literally"),
        "{said}"
    );
}

/// A search outside the reach is refused in the words every other tool refuses in.
#[tokio::test]
async fn a_path_outside_the_reach_is_refused() {
    let dir = tree("grep-reach");
    let said = ask(&dir, "grep", json!({ "pattern": "Kernel", "path": "/etc" })).await;

    assert!(
        said.contains("/etc") && !said.contains(":1:"),
        "nothing outside the working directory is searched: {said}"
    );
}

#[tokio::test]
async fn glob_lists_what_matches_in_order() {
    let dir = tree("glob-order");
    let said = ask(&dir, "glob", json!({ "pattern": "**/*.rs" })).await;

    assert_eq!(said, "2 path(s)\nsrc/app/keys.rs\nsrc/kernel.rs");

    // `*` crosses a separator, so the short spelling finds the same files
    let short = ask(&dir, "glob", json!({ "pattern": "*.rs" })).await;
    assert_eq!(short, said);
}

#[tokio::test]
async fn glob_says_when_nothing_matches() {
    let dir = tree("glob-nothing");
    let said = ask(&dir, "glob", json!({ "pattern": "**/*.py" })).await;

    assert_eq!(said, "nothing matches `**/*.py` under .");
}

/// Two identical searches answer identically, which a parallel walker would not promise.
///
/// note: not a pedantry. The answer becomes a context item, and two items that differ only in the
/// order of their lines are two items nobody can diff, priced twice.
#[tokio::test]
async fn the_same_search_twice_is_the_same_answer() {
    let dir = scratch("grep-stable");
    for n in 0..40 {
        put(&dir, &format!("f{n}.rs"), "pub struct Kernel;\n");
    }

    let once = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    let twice = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    assert_eq!(once, twice);
}

/// Both of them read files, so both of them ride the capability that says so.
///
/// note: the whole point of the pair. Finding a symbol used to mean `shell`, which subsumes every
/// other capability - so a session that only wanted to be asked about a repository had to hand
/// over the one permission that answers for everything.
#[tokio::test]
async fn finding_things_costs_read_and_not_shell() {
    let dir = tree("search-capability");
    for tool in tools(&dir) {
        let spec = tool.spec();
        if spec.id != "grep" && spec.id != "glob" {
            continue;
        }
        assert_eq!(
            spec.capabilities,
            vec![nachalnik::Capability::Read],
            "`{}` should need reading and nothing else",
            spec.id
        );
    }
}

/// `files_only` answers where a pattern lives, for a fraction of what the lines cost.
///
/// note: the live run this came from. A model opened with `grep tools` over the whole tree, got a
/// hundred lines and 3,216 tokens of changelog prose describing states the code has left, and
/// spent the rest of its turn chasing a sentence it found there. The lines were the wrong answer
/// to the question it was asking, which was *where is this registered*.
#[tokio::test]
async fn files_only_says_where_a_pattern_lives_rather_than_what_it_matched() {
    let dir = tree("grep-files-only");
    // one file matches twice and one once, so the order is a fact rather than a coincidence
    put(
        &dir,
        "src/app/keys.rs",
        "// the Kernel is next door\nfn go() {}\n",
    );

    let said = ask(
        &dir,
        "grep",
        json!({ "pattern": "Kernel", "files_only": true }),
    )
    .await;

    assert_eq!(
        said,
        "2 file(s) match, 3 match(es) in all · 3 file(s) searched\nsrc/kernel.rs: 2\nsrc/app/keys.rs: 1",
        "most matches first, and not one line of what they said"
    );

    // and it is the same search: the lines are there when they are asked for
    let lines = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;
    assert!(
        lines.contains("src/kernel.rs:1:pub struct Kernel;"),
        "{lines}"
    );
}

/// The saving is the point, and it is what the live run measured: a broad pattern cost 3,216
/// tokens of lines for a question the file names would have answered.
#[tokio::test]
async fn files_only_costs_a_fraction_of_what_the_lines_cost() {
    let dir = tree("grep-files-only-size");
    let many: String = (0..60).map(|n| format!("let x{n} = Kernel;\n")).collect();
    put(&dir, "src/many.rs", &many);

    let listed = ask(
        &dir,
        "grep",
        json!({ "pattern": "Kernel", "files_only": true }),
    )
    .await;
    let lines = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;

    assert_eq!(
        listed.lines().count(),
        4,
        "a header and three files: {listed}"
    );
    assert!(
        listed.len() * 4 < lines.len(),
        "{} bytes against {}",
        listed.len(),
        lines.len()
    );
    // and the file it came to find is at the top, because that is where the question points
    assert!(
        listed
            .lines()
            .nth(1)
            .is_some_and(|line| line == "src/many.rs: 60"),
        "{listed}"
    );
}

/// What `files_only` skips and counts is what an ordinary search does.
#[tokio::test]
async fn files_only_honours_the_path_rules_like_any_other_search() {
    let dir = tree("grep-files-only-barred");
    put(&dir, ".env", "TOKEN=Kernel-of-a-secret\n");

    let said = ask(
        &dir,
        "grep",
        json!({ "pattern": "Kernel", "files_only": true }),
    )
    .await;
    assert!(!said.contains(".env"), "{said}");
    assert!(
        said.contains("skipped: 1 file(s) a path rule says to ask about"),
        "{said}"
    );
}

/// A file with nothing in it for the pattern is not a file that matched.
#[tokio::test]
async fn files_only_says_which_empty_it_is_too() {
    let dir = tree("grep-files-only-nothing");
    let said = ask(
        &dir,
        "grep",
        json!({ "pattern": "Compactor", "files_only": true }),
    )
    .await;

    assert_eq!(said, "no matches for `Compactor` in . · 3 file(s) searched");
}

/// An argument a model quoted is read, and one nobody can read stops the search.
///
/// note: the scar is `introspect::log`'s, where `take: "3"` was swallowed by a bare `as_u64` and
/// the tool answered as though it had been asked for the default. Here the default for
/// `files_only` is the *expensive* answer, so a swallowed `"true"` costs three thousand tokens
/// and says nothing about why.
#[tokio::test]
async fn a_quoted_argument_is_read_and_an_unreadable_one_is_refused() {
    let dir = tree("grep-arguments");

    // quoted, which is a spelling rather than a mistake
    let said = ask(
        &dir,
        "grep",
        json!({ "pattern": "Kernel", "files_only": "true" }),
    )
    .await;
    assert!(said.starts_with("2 file(s) match"), "{said}");

    let context = ask(
        &dir,
        "grep",
        json!({ "pattern": "Kernel", "path": "src/app/keys.rs", "context": "1" }),
    )
    .await;
    assert!(
        context.contains("src/app/keys.rs-2-fn go() {}"),
        "{context}"
    );

    // and a word where a number belongs is not a search that quietly did something else
    for (args, what) in [
        (
            json!({ "pattern": "Kernel", "context": "some" }),
            "`context` is a whole number",
        ),
        (
            json!({ "pattern": "Kernel", "files_only": "yes" }),
            "`files_only` is true or false",
        ),
    ] {
        let refused = ask(&dir, "grep", args).await;
        assert!(refused.starts_with(what), "{refused}");
        assert!(
            refused.contains("Nothing was searched"),
            "it has to say the search did not happen: {refused}"
        );
    }
}

/// Asking for more context than the answer gives is said out loud, not clamped in silence.
///
/// note: watched live. A model asked for 20 lines either side, got ten, asked again for 25 and
/// got the same answer - a request spent on a ceiling nothing had mentioned. The same lesson as
/// the compaction marker one tool along: an answer that does not say what it did with your
/// argument reads as an answer to the argument you gave.
#[tokio::test]
async fn a_context_wider_than_the_answer_gives_says_so() {
    let dir = scratch("grep-context-clamped");
    put(
        &dir,
        "a.rs",
        &format!("{}Kernel\n{}", "one\n".repeat(30), "two\n".repeat(30)),
    );

    let said = ask(&dir, "grep", json!({ "pattern": "Kernel", "context": 25 })).await;
    assert!(
        said.contains(
            "context: 10 lines either side is the most this answers with, and you asked for 25"
        ),
        "{}",
        said.lines().take(3).collect::<Vec<_>>().join(" | ")
    );
    // ten either side, and the line itself
    assert_eq!(said.lines().filter(|l| l.starts_with("a.rs")).count(), 21);

    // and nothing is said when nothing was clamped
    let inside = ask(&dir, "grep", json!({ "pattern": "Kernel", "context": 2 })).await;
    assert!(
        !inside.contains("is the most this answers with"),
        "{inside}"
    );
}

/// Lines that will not fit are answered as the files they were in, not as the first few thousand
/// bytes of them.
///
/// note: the shape that prompted it. Four broad searches in one turn, each capped at a hundred
/// matches and each still filling its whole byte limit, put forty thousand tokens into a context
/// in one step - and a capped lines answer is filled from wherever the walk started, so most of
/// what it cost was lines nobody had asked about. The advice the capped answer already gives is
/// `files_only`; this takes it rather than printing it.
#[tokio::test]
async fn lines_that_will_not_fit_come_back_as_the_files_they_were_in() {
    let dir = tree("grep-too-wide");
    // sparse matches in wide files, which is the shape that costs: consecutive matches share
    // their context lines and add nothing, and a match every eighth line pulls six more with it
    let filler = format!("// {}\n", "n".repeat(190));
    let hit = format!("let x = Kernel; // {}\n", "n".repeat(190));
    let block = format!("{}{hit}", filler.repeat(7));
    for file in 0..12 {
        put(&dir, &format!("src/wide{file}.rs"), &block.repeat(20));
    }

    // with `context`, which is what makes a lines answer big enough to matter: a hundred matches
    // at two hundred characters is deliberately under the byte limit, and the same hundred with
    // three lines either side is seven times that. Both of the searches that prompted this asked
    // for context
    let said = ask(&dir, "grep", json!({ "pattern": "Kernel", "context": 3 })).await;

    assert!(
        said.contains("more than this answers with, so here is where they are"),
        "{said}"
    );
    assert!(
        said.contains("src/wide0.rs: "),
        "and where that is, by file: {said}"
    );
    assert!(!said.contains("// nnn"), "not the lines themselves: {said}");
    assert!(
        said.len() < 4_000,
        "and it is small enough to be worth the swap: {} bytes",
        said.len()
    );
    // the two ways out, named, because a model handed files where it asked for lines asks again
    assert!(
        said.contains("Narrow the pattern or name one of these files"),
        "{said}"
    );
}

/// An answer that fits is left exactly as it was.
#[tokio::test]
async fn lines_that_fit_are_still_the_lines() {
    let dir = tree("grep-fits");
    let said = ask(&dir, "grep", json!({ "pattern": "Kernel" })).await;

    assert!(
        said.contains("src/kernel.rs:1:pub struct Kernel;"),
        "{said}"
    );
    assert!(!said.contains("here is where they are"), "{said}");
}
