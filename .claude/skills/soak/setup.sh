#!/bin/bash
# Prepares a soak: a scratch worktree, the registry on disk for offline builds, and the settings the
# segments run under. The key is not written here; see SKILL.md.
#
#   setup.sh MODEL
set -eu
SOAK=${SOAK:?set SOAK}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
model=$1
mkdir -p "$SOAK/segments" "$SOAK/rec"
[ -d "$SOAK/wt" ] || git -C "$REPO" worktree add --detach "$SOAK/wt" HEAD
(cd "$SOAK/wt" && cargo fetch --quiet)
MODEL=$model CARGO_HOME=${CARGO_HOME:-$HOME/.cargo} RUSTUP=${RUSTUP_HOME:-$HOME/.rustup} \
    GITDIR=$REPO/.git OUT=$SOAK/soak.json python3 - <<'EOF'
import json, os
json.dump({
    "model": os.environ["MODEL"],
    "requests": 0,
    "tools": ["fs", "shell", "context", "log", "setup", "fork"],
    "allow": ["fs", "exec", "context", "log", "setup", "fork"],
    "on-ask": "deny",
    "compact": 0.6,
    "sandbox-read": [os.environ["CARGO_HOME"], os.environ["RUSTUP"], os.environ["GITDIR"]],
    "system": "You are a software engineer working in the Rust workspace in the current "
        "directory, for a person who is not watching and cannot answer questions. Do what each "
        "message asks with the tools you have, and say plainly when something cannot be done. "
        "The network is closed; cargo works with --offline, and CARGO_TARGET_DIR is already set, "
        "so do not set it. Your context is small: keep it tidy with the `context` tool, and "
        "write down what you will still need before you put a result away.",
}, open(os.environ["OUT"], "w"), indent=1)
EOF
echo "$SOAK/soak.json"
