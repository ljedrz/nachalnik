#!/usr/bin/env bash
# Shows Claude the plain-writing rule right after anything changes a document, and asks for the
# edit to be checked against it.
#
# note: the documents are every tracked Markdown file outside `.claude/` - the READMEs, the guides,
# the changelogs, SECURITY.md, CONTRIBUTING.md and the rest - since all of them are read by people.
# Edit and Write are recognised by their path. A Bash command is recognised by a document's hash
# having changed since the last time this looked, as `postponed.sh` does; the hashes are kept in
# the git directory, so nothing is left in the tree.
#
# note: a change of HEAD - a checkout, a rebase, a pull - changes documents nobody edited here, so
# when HEAD has moved the hashes are taken again and nothing is said.
set -euo pipefail

root="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}"
cd "$root"

input="$(cat)"
tool="$(jq -r '.tool_name // empty' <<<"$input")"
path="$(jq -r '.tool_input.file_path // empty' <<<"$input")"

stamp="$(git rev-parse --absolute-git-dir)/plain.sha256"
head="$(git rev-parse HEAD 2>/dev/null || echo none)"
now="$(
    echo "HEAD $head"
    git ls-files -z -- '*.md' ':!:.claude/*' | xargs -0 -r sha256sum
)"
before="$(cat "$stamp" 2>/dev/null || true)"
echo "$now" >"$stamp"

case "$tool" in
    Edit | Write | MultiEdit)
        [ -n "$path" ] || exit 0
        relative="$(realpath -m --relative-to="$root" "$path")"
        case "$relative" in
            .claude/* | ../* | target/*) exit 0 ;;
            *.md) changed="$relative" ;;
            *) exit 0 ;;
        esac
        ;;
    *)
        # no baseline yet, or HEAD moved: take the hashes and say nothing
        [ -n "$before" ] || exit 0
        [ "$(head -n1 <<<"$before")" = "HEAD $head" ] || exit 0
        changed="$(
            # `diff` exits 1 when there are differences, which is the case this is looking for
            { diff <(tail -n +2 <<<"$before") <(tail -n +2 <<<"$now") || true; } |
                sed -n 's/^> [0-9a-f]*  //p' | sort -u | paste -sd ' '
        )"
        [ -n "$changed" ] || exit 0
        ;;
esac

jq -n --arg changed "$changed" '{
  decision: "block",
  reason: ("Documentation changed (" + $changed + "). Check every passage you wrote or touched against AGENTS.md'"'"'s rule: write plainly. Use ordinary words and short sentences, say things directly, and write the way you would explain it to a colleague at the next desk. No riddles, no personified code, no aphorisms, no sentences about the text itself. If a sentence has to be read twice, rewrite it. If what you wrote complies, carry on; if not, correct it now.")
}'
