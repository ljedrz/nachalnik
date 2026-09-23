//! The ceiling on what `shell` keeps of a command's output, measured as memory rather than as
//! bytes in the answer.
//!
//! note: a file of its own because the figure is the process's peak, which every other test in a
//! binary would add to.

#![cfg(target_os = "linux")]

mod common;

use std::sync::Arc;

use kamchatka::tools::{Careful, KEPT, Limits, Shell};
use nachalnik::{OutputSink, Tool, test::call};
use serde_json::json;

/// The most this process has held at once, in bytes.
fn peak() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").expect("linux says");
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM"))
        .expect("a peak");
    let kb: usize = line
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .expect("a number");

    kb * 1024
}

/// Output that never ends a line is held to the ceiling while it is being read, not only after.
///
/// note: the ceiling was checked when a read returned, and a read of a line with no end returned
/// only when the heartbeat stopped it - a tenth of a second at the speed of a pipe, which held
/// hundreds of megabytes against a ceiling of eight. The bound here is a few ceilings, well clear
/// of both the fix and the fault.
#[tokio::test(flavor = "multi_thread")]
async fn a_line_that_never_ends_is_not_held_past_the_ceiling() {
    let shell = Shell {
        workdir: common::scratch("ceiling"),
        extra: Vec::new(),
        readable: Vec::new(),
        policy: Arc::new(Careful::new()),
        confiner: None,
        limits: Limits::default(),
    };
    let args = json!({ "call": { "action": "run", "cmd": "head -c 2000000000 /dev/zero" } });

    let before = peak();
    shell
        .invoke(&call("c1", "shell", args), OutputSink::disconnected())
        .await
        .expect("the tool answers either way");
    let held = peak().saturating_sub(before);

    assert!(
        held < 8 * KEPT,
        "reading two gigabytes with no newline in them held {} MB",
        held >> 20
    );
}
