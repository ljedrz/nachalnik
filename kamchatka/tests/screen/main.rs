//! Tests that draw the screen and read it back.
//!
//! note: A terminal program whose tests only checked its state would be testing the half that
//! nobody looks at. These render into a `TestBackend` and assert on the characters that come out,
//! which is the same thing a person sitting in front of it would be doing.
//!
//! note: The model is a `ScriptedProvider` and the tools do nothing, so none of this touches a
//! network or a file. What is under test is the wiring: that a key press reaches the kernel, that
//! the kernel's answer reaches the screen, and that what the screen says about the next request
//! is what the next request would actually contain.

// note: `tests/screen/main.rs` rather than `tests/screen.rs`, because a crate root looks for its
// submodules beside itself - so the ten below would have to be `tests/chat.rs`, each of them its
// own test binary. Cargo takes a directory with a `main.rs` in it as one target named for the
// directory, which is what keeps these ten one binary called `screen`. The path to `common` is
// the price: it is shared with every other test in here and stays where they can all reach it.
#[path = "../common/mod.rs"]
mod common;

mod chat;
mod compaction;
mod context;
mod frame;
mod harness;
mod permissions;
mod session;
mod status;
mod trace;
mod turns;
