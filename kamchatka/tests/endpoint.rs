//! The endpoint's two settings read from the environment, as a caller of the library reads them.
//!
//! note: these are the library's own functions, which the binary does not call in the same way:
//! `configured_limit` is what `live` hands a provider, and `attribution` is what every client
//! built by `endpoint` is handed. Each test binary is its own process, so the variables set here
//! reach nothing else; within this one they are set and read by a single test, so no two tests
//! race on them.

use kamchatka::endpoint::{attribution, configured_limit};
use nachalnik_providers::Attribution;

/// Attribution is on unless `KAMCHATKA_NO_ATTRIBUTION` is set, and a configured limit is the
/// figure `KAMCHATKA_CONTEXT_LIMIT` holds when it is a whole number above 0, and nothing otherwise.
///
/// note: one test rather than several, because the environment is shared by every test in the
/// binary and these run in parallel: each of these reads a variable the others write.
#[test]
fn the_endpoint_settings_read_the_environment_as_documented() {
    // SAFETY: this is the only test in this binary, so nothing else reads or writes the
    // environment while these run.
    unsafe {
        std::env::remove_var("KAMCHATKA_NO_ATTRIBUTION");
        std::env::remove_var("KAMCHATKA_CONTEXT_LIMIT");
    }

    // unset: the program names itself, with its own address and what kind of program it is
    let named = attribution();
    assert_ne!(named, Attribution::default(), "unset, nothing is named");
    assert_eq!(
        named.url, "https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka",
        "the address it names"
    );
    assert_eq!(named.title, "kamchatka");
    assert_eq!(named.categories, ["cli-agent", "programming-app"]);

    // set, to anything at all: nothing is named or sent
    unsafe { std::env::set_var("KAMCHATKA_NO_ATTRIBUTION", "1") };
    assert_eq!(
        attribution(),
        Attribution::default(),
        "set, something is named"
    );
    unsafe { std::env::remove_var("KAMCHATKA_NO_ATTRIBUTION") };

    // unset: no limit is set by hand
    assert_eq!(configured_limit(), None, "unset, a limit is read");

    // a whole number above 0 is the limit
    unsafe { std::env::set_var("KAMCHATKA_CONTEXT_LIMIT", "128000") };
    assert_eq!(configured_limit(), Some(128_000));

    // `0`, and a value that is not a number, are not limits: `None` here, and refused at startup
    // by the program, which this function does not do
    for wrong in ["0", "abc", "-5", ""] {
        unsafe { std::env::set_var("KAMCHATKA_CONTEXT_LIMIT", wrong) };
        assert_eq!(configured_limit(), None, "`{wrong}` is read as a limit");
    }

    unsafe { std::env::remove_var("KAMCHATKA_CONTEXT_LIMIT") };
}
