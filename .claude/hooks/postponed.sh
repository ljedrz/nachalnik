#!/usr/bin/env bash
# Shows Claude the rule for POSTPONED.md right after anything changes it, and asks for the edit to
# be checked against it.
#
# note: Edit and Write are recognised by their path. A Bash command - a sed, a heredoc, a script -
# is recognised by the file having changed since the last time this looked, which is the only way
# to catch an edit nothing names; the hash it compares against is kept in the git directory, so
# nothing is left in the tree.
set -euo pipefail

root="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}"
file="$root/POSTPONED.md"
[ -f "$file" ] || exit 0

input="$(cat)"
tool="$(jq -r '.tool_name // empty' <<<"$input")"
path="$(jq -r '.tool_input.file_path // empty' <<<"$input")"

stamp="$(git -C "$root" rev-parse --absolute-git-dir)/postponed.sha256"
now="$(sha256sum "$file" | cut -d' ' -f1)"
before="$(cat "$stamp" 2>/dev/null || true)"
echo "$now" >"$stamp"

case "$tool" in
    Edit | Write | MultiEdit) [ "$(realpath -m "$path")" = "$(realpath "$file")" ] || exit 0 ;;
    *) [ -n "$before" ] && [ "$before" != "$now" ] || exit 0 ;;
esac

jq -n '{
  decision: "block",
  reason: "POSTPONED.md changed. Check every entry you touched against the rule in its header: an entry says only what is still open - what it is, why it waits, what would unblock it. No finished work, no record of what was tested or checked, no history; those go in the commit message. A fix that closes an entry removes it. If what you wrote complies, carry on; if not, correct it now."
}'
