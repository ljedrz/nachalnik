//! When an event happened, on the clock a person reads.
//!
//! note: here rather than beside the pane that draws it, because the stamp is not only painted -
//! it is part of what a search over the trace matches on, and the filtering lives with the data.
//! A pane that searched one string and displayed another would be a pane that finds nothing where
//! it says there is something.

use std::{sync::OnceLock, time::SystemTime};

use time::{OffsetDateTime, UtcOffset};

/// The local offset from UTC, in seconds, as it was before this program had more than one thread.
///
/// note: captured once at startup rather than read per event, and that is a soundness requirement
/// rather than a saving. Working out a local time means asking libc, which reads the process
/// environment; another thread setting an environment variable at the same moment is undefined
/// behaviour, so the `time` crate refuses to answer at all once a program is threaded. `main`
/// asks before it builds the runtime, which is the one moment there is nobody to race.
///
/// note: `None` twice over, and they mean different things that the pane renders the same way.
/// Not yet set is a headless build or a test that never went through `main`; set to `None` is a
/// platform that would not say. Either way the clock falls back to UTC and says so, because a
/// column of times that is silently two hours out is worse than one that admits which zone it is
/// in.
static LOCAL_OFFSET: OnceLock<Option<i32>> = OnceLock::new();

/// Records the local offset. Call once, from a program that has not started any threads yet.
pub fn note_local_offset(seconds: Option<i32>) {
    let _ = LOCAL_OFFSET.set(seconds);
}

/// When something happened, on the clock a person reads: the date, the time of day, and whether
/// the two are local or UTC.
///
/// note: `time` does the calendar rather than three divisions here, which is a reversal of what
/// this said when it only had to produce a time of day. Turning seconds into `HH:MM:SS` really is
/// arithmetic; turning them into a *date* is leap years, and the crate is already compiled for
/// this build. The date matters because a session can outlast a day, and a pane that showed
/// `00:15` against two different Tuesdays would be worse than one showing no clock at all.
///
/// note: the offset arrives as a plain `i32` so that nothing below the screen has to know about
/// time zones to record when something happened - `app` keeps a `SystemTime` and no more.
pub fn read_off(wall: SystemTime) -> Option<When> {
    let local = LOCAL_OFFSET.get().copied().flatten();
    let offset = local
        .and_then(|seconds| UtcOffset::from_whole_seconds(seconds).ok())
        .unwrap_or(UtcOffset::UTC);
    let at = OffsetDateTime::from(wall).to_offset(offset);

    Some(When {
        date: format!(
            "{:04}-{:02}-{:02}",
            at.year(),
            u8::from(at.month()),
            at.day()
        ),
        time: format!("{:02}:{:02}:{:02}", at.hour(), at.minute(), at.second()),
        local: local.is_some(),
    })
}

/// A rendered wall clock.
pub struct When {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `HH:MM:SS`.
    pub time: String,
    /// Whether that is the local zone, or UTC because the local one could not be had.
    pub local: bool,
}
