//! The ceiling on what `shell` keeps of a command's output, measured as memory and as what is
//! kept of a stream that is not text.
//!
//! note: a file of its own because the figure is the process's peak, which every other test in a
//! binary would add to - and the two here take turns, through `ONE_AT_A_TIME`, for the same
//! reason.

#![cfg(target_os = "linux")]

mod common;

use std::sync::Arc;

use kamchatka::tools::{Careful, KEPT, Limits, Shell};
use nachalnik::{OutputSink, Tool, test::call};
use serde_json::json;

/// Held for the whole of each test, so that neither adds to the peak the other measures.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _turn = ONE_AT_A_TIME.lock().await;
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

/// Output that is not text is held to the ceiling as it is kept, not as it arrived.
///
/// note: a byte that is not UTF-8 is kept as the three bytes of `�`, and both streams were held to
/// the ceiling by the bytes that arrived: a line of standard output just under it, or standard
/// error up to it, came back at nearly three times the ceiling.
#[tokio::test(flavor = "multi_thread")]
async fn output_that_is_not_text_is_held_to_the_ceiling_as_it_is_kept() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let shell = Shell {
        workdir: common::scratch("unkept"),
        extra: Vec::new(),
        readable: Vec::new(),
        policy: Arc::new(Careful::new()),
        confiner: None,
        limits: Limits::default(),
    };
    let bytes = format!("head -c {} /dev/zero | tr '\\0' '\\377'", KEPT - 1024);

    for cmd in [format!("{bytes}; echo"), format!("{bytes} >&2")] {
        let args = json!({ "call": { "action": "run", "cmd": cmd } });
        let output = shell
            .invoke(&call("c1", "shell", args), OutputSink::disconnected())
            .await
            .expect("the tool answers either way");
        let kept = output.content.to_text().len();

        assert!(
            kept <= KEPT + 4096,
            "`{cmd}` kept {} MB against a ceiling of {}",
            kept >> 20,
            KEPT >> 20
        );
    }
}
