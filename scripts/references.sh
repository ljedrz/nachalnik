#!/usr/bin/env bash
#
# Checks that what the prose names still exists. A backticked file in a document or a comment has
# to be a file in the repository; a backticked test name, or a path into this workspace's own
# items - `Vigil::judge`, `Config::max_requests_per_turn` - has to be something the code declares.
#
#   scripts/references.sh
#
# note: shallow on purpose. A name is looked for, not resolved: `Kernel::step` passes if anything
# anywhere declares a `step`. What it catches is the common case - a rename or a deletion that left
# a sentence pointing at nothing - and it is quick enough to run before every commit.
#
# note: what it cannot tell apart from a stale reference is a name mentioned on purpose - the
# layout a test directory was chosen over, a file an example writes. Those are listed in
# `scripts/references.allow`, beside the file that mentions them. Changelogs are not read at all:
# what they name was true on the day they say.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# every file, under every tail of its path: `a/b/c.rs` answers to `c.rs`, `b/c.rs` and itself
git ls-files --cached --others --exclude-standard | awk -F/ '{
    for (i = 1; i <= NF; i++) {
        tail = $i
        for (j = i + 1; j <= NF; j++) tail = tail "/" $j
        print tail
    }
}' | sort -u > "$scratch/files"

ident='[A-Za-z_][A-Za-z0-9_]*'
item="(fn|mod|struct|enum|trait|type|const|static|macro_rules!)[[:space:]]+$ident"
visibility='^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?'
field="$visibility[a-z_][a-z0-9_]*: "
variant='^[[:space:]]+[A-Z][A-Za-z0-9]*[[:space:]]*[,({=]'

# every name the code declares something under: items, fields, and variants
{
    git grep --untracked -hoE "$item" -- '*.rs' | awk '{ print $NF }'
    git grep --untracked -hoE "$field" -- '*.rs' | sed -E "s/$visibility//; s/: \$//"
    git grep --untracked -hoE "$variant" -- '*.rs' |
        sed -E 's/^[[:space:]]+//; s/[[:space:]]*[,({=]$//'
} | sort -u > "$scratch/names"

# what a path has to start with to be a path into this workspace rather than into `std` or a
# dependency: one of its types, traits or modules, one of its crates, or `crate` itself
{
    git grep --untracked -hoE "(mod|struct|enum|trait|type)[[:space:]]+$ident" -- '*.rs' |
        awk '{ print $NF }'
    git grep --untracked -hE '^name = ' -- '*Cargo.toml' | sed -E 's/^name = "(.*)"$/\1/; s/-/_/g'
    printf '%s\n' crate self super
} | sort -u > "$scratch/roots"

# every backticked word in a comment, and in every document but a changelog
{
    git grep --untracked -nE '^[[:space:]]*//.*`' -- '*.rs'
    git grep --untracked -nE '`' -- '*.md' ':!:*CHANGELOG.md'
} | awk -F: '{
    where = $1 ":" $2
    text = substr($0, length(where) + 2)
    while (match(text, /`[^` ]+`/)) {
        print where "\t" substr(text, RSTART + 1, RLENGTH - 2)
        text = substr(text, RSTART + RLENGTH)
    }
}' > "$scratch/references"

awk -F'\t' \
    -v files="$scratch/files" \
    -v names="$scratch/names" \
    -v roots="$scratch/roots" \
    -v allowed="scripts/references.allow" '
    BEGIN {
        while ((getline line < files) > 0) file[line] = 1
        while ((getline line < names) > 0) name[line] = 1
        while ((getline line < roots) > 0) root[line] = 1
        while ((getline line < allowed) > 0) {
            if (line ~ /^#/ || line !~ /[^[:space:]]/) continue
            split(line, part, /[[:space:]]+/)
            allow[part[1] "\t" part[2]] = 1
        }
    }
    {
        where = $1
        token = $2
        path = where
        sub(/:[0-9]+$/, "", path)
        if ((path "\t" token) in allow) next

        if (token ~ /^[A-Za-z0-9_.\/-]+\.(rs|md|toml|sh|py|html|yml)$/) {
            named = token
            while (sub(/^\.\.?\//, "", named)) {}
            if (!(named in file)) {
                print where ": `" token "` is not a file in this repository"
                wrong++
            }
            next
        }

        # a path into the workspace, or on its own a name long enough to be a test
        item = token
        sub(/\(\)$/, "", item)
        if (item ~ /^([A-Za-z0-9_]+::)+[A-Za-z_][A-Za-z0-9_]*$/) {
            n = split(item, segment, "::")
            if (!(segment[1] in root)) next
        } else if (item ~ /^[a-z][a-z0-9]*(_[a-z0-9]+)(_[a-z0-9]+)(_[a-z0-9]+)+$/) {
            n = split(item, segment, "::")
        } else {
            next
        }
        if (!(segment[n] in name)) {
            print where ": `" token "` names nothing the code declares"
            wrong++
        }
    }
    END {
        if (wrong) {
            print ""
            print wrong " reference(s) to nothing. A name mentioned on purpose - one that was"
            print "rejected, or one a program makes - goes in scripts/references.allow."
            exit 1
        }
    }' "$scratch/references"
