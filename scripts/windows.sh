#!/usr/bin/env bash
#
# Checks the workspace for Windows from here, in the configurations CI's Windows job builds. What
# it catches is what the compiler sees: a helper whose only callers are `#[cfg(unix)]`, an import
# only a unix path reaches, a type that does not exist there. What it does not catch is anything
# Windows does differently at run time - a refused connection there takes seconds rather than
# nothing - which only CI's run of the tests can show.
#
#   scripts/windows.sh
#
# Needs `cargo install --locked cargo-xwin`. The first run downloads Microsoft's CRT and SDK, which
# is under Microsoft's licence: it asks, or `XWIN_ACCEPT_LICENSE=1` says you have read it.

set -uo pipefail

if ! cargo xwin --version > /dev/null 2>&1; then
    echo "needs cargo-xwin: cargo install --locked cargo-xwin" >&2
    exit 2
fi

# the same denial CI builds under, so that a warning fails here as it does there
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"

target=x86_64-pc-windows-msvc
failed=()

check() {
    echo "== $*"
    if ! cargo xwin check -q --locked --target "$target" "$@"; then
        failed+=("$*")
    fi
}

check --workspace
check --workspace --all-features --all-targets
check -p nachalnik --no-default-features
check -p nachalnik-mcp --no-default-features
check -p kamchatka --no-default-features --all-targets
check -p kamchatka --no-default-features --features mcp --all-targets
check -p nachalnik-providers --all-targets --no-default-features --features gemini
check -p nachalnik-providers --all-targets --no-default-features --features openai,conformance
check -p nachalnik-providers --all-targets --no-default-features

if [ ${#failed[@]} -gt 0 ]; then
    echo >&2
    echo "failed for $target:" >&2
    printf '  %s\n' "${failed[@]}" >&2
    exit 1
fi
echo "every configuration checks for $target"
