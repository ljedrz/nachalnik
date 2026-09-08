//! What the test suites share: a place to put files that is not `/tmp`.

use std::path::PathBuf;

/// A directory of this test's own, emptied first, under the one cargo hands the test binaries.
///
/// note: `CARGO_TARGET_TMPDIR` rather than `std::env::temp_dir()`, which is what these used to
/// use. Every one of them cleared its directory on the way in rather than on the way out - a test
/// that fails is a test whose leavings you want to look at - and the name carried the process
/// identifier, so a fresh one arrived with every run and none of them ever left. Thirty runs of
/// this suite had put eight hundred and ten directories in `/tmp`, which is a real cost to
/// somebody else's machine and an odd thing for the tidy program to do. Cargo hands integration
/// tests a directory for exactly this, it is under `target/`, and `cargo clean` sweeps it.
///
/// note: no process identifier in the name, now that they are not accumulating. Cargo runs one
/// test binary at a time, so a name only has to be unique within its own suite, and a stable name
/// is one somebody debugging a failure can find - which was the reason for clearing on the way in
/// rather than the way out, and is worth keeping.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory to work in");

    dir
}
