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
/// Not yet set is a test or an embedder that never went through `main`; set to `None` is a
/// system whose zone could not be read. Either way the clock falls back to UTC and says so, because
/// a column of times that is silently two hours out is worse than one that admits which zone it is
/// in.
static LOCAL_OFFSET: OnceLock<Option<i32>> = OnceLock::new();

/// Records the local offset. Call once, from a program that has not started any threads yet.
pub fn note_local_offset(seconds: Option<i32>) {
    let _ = LOCAL_OFFSET.set(seconds);
}

/// When something happened, on the clock a person reads: the date, the time of day, and whether
/// the two are local or UTC.
///
/// note: `time` does the calendar rather than three divisions here. Turning seconds into
/// `HH:MM:SS` is arithmetic; turning them into a *date* is leap years, and the crate is already
/// compiled for this build. The date matters because a session can outlast a day, and a pane that
/// showed `00:15` against two different Tuesdays would be worse than one showing no clock at all.
///
/// note: the offset arrives as a plain `i32` so that nothing below the screen has to know about
/// time zones to record when something happened - `app` keeps a `SystemTime` and no more.
///
/// note: a `When` rather than an `Option` of one, because there is always an answer: a zone that
/// cannot be read is UTC, said as UTC. And `local` is whether the offset *was used*, not whether
/// one was recorded - an offset `time` refuses falls back to UTC like a missing one, and a clock
/// that said it was local while showing UTC would be the thing the fallback exists to prevent.
pub fn read_off(wall: SystemTime) -> When {
    read_at(wall, LOCAL_OFFSET.get().copied().flatten())
}

/// The same, against an offset given rather than the one recorded - which is the whole of the
/// rule, and a global a test could set once for every test there is.
fn read_at(wall: SystemTime, recorded: Option<i32>) -> When {
    let local = recorded.and_then(|seconds| UtcOffset::from_whole_seconds(seconds).ok());
    let at = OffsetDateTime::from(wall).to_offset(local.unwrap_or(UtcOffset::UTC));

    When {
        date: format!(
            "{:04}-{:02}-{:02}",
            at.year(),
            u8::from(at.month()),
            at.day()
        ),
        time: format!("{:02}:{:02}:{:02}", at.hour(), at.minute(), at.second()),
        local: local.is_some(),
    }
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    /// A clock is local only where the local offset was used, and UTC, said as UTC, otherwise.
    ///
    /// note: an offset `time` refuses - past a day either way - fell back to UTC and was still
    /// reported as local, which is a column of times that are out by the zone and do not say so.
    #[test]
    fn a_clock_says_local_only_where_it_is() {
        let noon = UNIX_EPOCH + Duration::from_secs(12 * 3600);

        let east = read_at(noon, Some(3600));
        assert!(east.local);
        assert_eq!(east.time, "13:00:00");

        for unusable in [None, Some(i32::MAX)] {
            let read = read_at(noon, unusable);
            assert!(!read.local, "{unusable:?} was said to be local");
            assert_eq!(read.time, "12:00:00", "{unusable:?}");
        }
    }

    /// The offset `main` notes is the one `read_off` reads, and it is what says the clock local.
    ///
    /// note: the global can only be set once per process, so this is the test that notes one, and
    /// `read_at` above is what covers the rest. A zone east of Greenwich is the case that matters:
    /// an offset nobody kept leaves every stamp in the run at UTC and says UTC, which is a quiet
    /// answer rather than a wrong one and so nothing else would catch.
    #[test]
    fn a_noted_offset_is_the_one_the_clock_reads() {
        note_local_offset(Some(2 * 3600));

        let east = read_off(UNIX_EPOCH + Duration::from_secs(12 * 3600));

        assert!(east.local, "a noted offset was not used");
        assert_eq!(east.time, "14:00:00");
    }
}
