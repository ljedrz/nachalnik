#!/usr/bin/env python3
"""Where the tools cost a model a call: every error result in a set of session snapshots, bucketed.

    friction.py SNAPSHOT.json...            the report
    friction.py --json SNAPSHOT.json...     every failed call, one JSON object a line

A snapshot is what `kamchatka --headless` writes at the end (`a session in ....json`): the items,
with every call's arguments on the assistant turn and every result's text on its own item. The
event log names results rather than copying them, so it cannot say what an error said.

A bucket is an error's text with the parts that vary taken out - backticked names, numbers, paths
- so that fifty `grep does not take X` are one row. Each row says how often, in how many sessions,
what the model did next (the same call again, a changed call to the same tool, or another
tool), and one example. Read the examples: the text is the tool's, the arguments are the model's,
and which of the two is wrong is the question.
"""

import collections
import json
import re
import sys


def op_of(tool, args):
    """The operation a call named, as `tool:action`, or the tool alone."""
    call = args.get("call") if isinstance(args, dict) else None
    inner = call if isinstance(call, dict) else args
    action = inner.get("action") if isinstance(inner, dict) else None
    return f"{tool}:{action}" if isinstance(action, str) else tool


def shape(text):
    """An error's text with what varies taken out."""
    first = text.strip().split("\n")[0][:400]
    first = re.sub(r"`[^`]*`", "`_`", first)
    first = re.sub(r"'[^']*'", "'_'", first)
    first = re.sub(r"\"[^\"]*\"", '"_"', first)
    first = re.sub(r"(/[\w.\-]+)+", "/_", first)
    first = re.sub(r"\d[\d,._]*", "N", first)
    return first


def calls_of(path):
    """Every call in one snapshot, in order, with its result."""
    items = json.load(open(path))["items"]
    results = {}
    for item in items:
        kind = item["kind"]
        if kind.get("kind") == "tool_result":
            results[kind["call"]] = (kind.get("is_error", False), item["content"].get("text") or "")
    out = []
    for item in items:
        kind = item["kind"]
        if kind.get("kind") != "assistant_message":
            continue
        for call in kind.get("tool_calls") or []:
            error, text = results.get(call["id"], (None, ""))
            out.append(
                {
                    "session": path,
                    "tool": call["tool"],
                    "op": op_of(call["tool"], call["args"]),
                    "args": call["args"],
                    "error": error,
                    "text": text,
                }
            )
    return out


def main(argv):
    dump = "--json" in argv
    paths = [a for a in argv if a != "--json"]
    ops = collections.Counter()
    failed_ops = collections.Counter()
    buckets = collections.defaultdict(list)
    for path in paths:
        calls = calls_of(path)
        for i, c in enumerate(calls):
            ops[c["op"]] += 1
            if not c["error"]:
                continue
            failed_ops[c["op"]] += 1
            after = calls[i + 1] if i + 1 < len(calls) else None
            if after is None:
                c["next"] = "ended"
            elif after["args"] == c["args"] and after["tool"] == c["tool"]:
                c["next"] = "same call"
            elif after["tool"] == c["tool"]:
                c["next"] = "changed call" + (", failed" if after["error"] else ", ok")
            else:
                c["next"] = "another tool"
            if dump:
                print(json.dumps(c))
            buckets[shape(c["text"])].append(c)
    if dump:
        return
    total = sum(ops.values())
    print(f"{len(paths)} sessions, {total} calls, {sum(failed_ops.values())} errors\n")
    print("by operation (calls, errors):")
    for op, n in ops.most_common():
        e = failed_ops[op]
        print(f"  {op:28} {n:6} {e:5}  {100 * e / n:5.1f}%")
    print("\nby error, most frequent first:")
    for text, cs in sorted(buckets.items(), key=lambda kv: -len(kv[1])):
        sessions = len({c["session"] for c in cs})
        nexts = collections.Counter(c.get("next") for c in cs)
        ops_hit = collections.Counter(c["op"] for c in cs)
        print(f"\n[{len(cs)} in {sessions} sessions] {text}")
        print("  ops: " + ", ".join(f"{o} {n}" for o, n in ops_hit.most_common(4)))
        print("  next: " + ", ".join(f"{k} {n}" for k, n in nexts.most_common()))
        ex = cs[0]
        print("  e.g. args: " + json.dumps(ex["args"])[:300])
        print("       said: " + ex["text"].strip().replace("\n", " ")[:400])


if __name__ == "__main__":
    main(sys.argv[1:])
