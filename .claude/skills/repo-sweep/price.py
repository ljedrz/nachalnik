#!/usr/bin/env python3
"""Stops a sweep run the moment its model is no longer listed as free.

usage: price.py MODEL [--every MINUTES]

Reads the endpoint's `/models` listing every `--every` minutes (default 10). When MODEL has any
nonzero price, is gone from the listing, or the listing cannot be read ten times in a row, it
stops `run.py` and then every sweep still running, and exits. Run it in the background beside
`run.py`, with the key sourced, for any run that is meant to cost nothing.

note: `run.py` first, so that nothing new starts, then each sweep by its `NAME.pid` with SIGTERM,
which makes kamchatka write its session: a sweep stopped this way can be resumed rather than run
again from the start.

note: it reads only the listing, and some free offers are not in it. A promotion that waives
a model's price leaves the listed price as it was, and an offer can end without the listing
changing. That happened once: the free route stopped answering new requests, and the sweeps on
connections already open went on being answered and billed at the listed price for five hours.
So whatever this says, a free endpoint that suddenly fails is a reason to stop and ask the person
to look at their balance, not an outage to wait out.
"""

import glob
import json
import os
import signal
import sys
import time
import urllib.request

import sweeps


def listed(model):
    base = os.environ.get("KAMCHATKA_BASE_URL", "https://openrouter.ai/api/v1").rstrip("/")
    key = os.environ.get("KAMCHATKA_API_KEY", "")
    request = urllib.request.Request(
        f"{base}/models", headers={"Authorization": f"Bearer {key}", "User-Agent": "repo-sweep"}
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        for entry in json.load(response).get("data", []):
            if entry.get("id") == model:
                return entry.get("pricing") or {}
    return None


def priced(pricing):
    """The prices in a listing that are not zero; OpenRouter and Vercel's gateway give strings."""
    out = {}
    for name, value in pricing.items():
        try:
            if float(value) != 0:
                out[name] = value
        except (TypeError, ValueError):
            pass
    return out


def stop(why):
    print(time.strftime("%H:%M"), "stopping the run:", why, flush=True)
    try:
        pid = int(open(os.path.join(sweeps.SWEEPS, "run.pid")).read())
        if sweeps.alive(pid):
            os.kill(pid, signal.SIGTERM)
    except (OSError, ValueError):
        pass
    stopped = []
    for path in glob.glob(os.path.join(sweeps.SWEEPS, "*.pid")):
        name = os.path.basename(path)[: -len(".pid")]
        if name == "run":
            continue
        try:
            pid = int(open(path).read())
        except (OSError, ValueError):
            continue
        if sweeps.alive(pid):
            os.kill(pid, signal.SIGTERM)
            stopped.append(name)
    print(time.strftime("%H:%M"), f"stopped {len(stopped)}:", " ".join(stopped), flush=True)


def main():
    args = sys.argv[1:]
    if not args or args[0].startswith("-"):
        sys.exit(__doc__)
    model = args[0]
    every = int(args[args.index("--every") + 1]) if "--every" in args else 10

    unread = 0
    while True:
        try:
            pricing = listed(model)
        except Exception as e:
            unread += 1
            print(time.strftime("%H:%M"), "could not read the listing:", e, flush=True)
            if unread >= 10:
                return stop("the listing could not be read ten times in a row")
        else:
            unread = 0
            if pricing is None:
                return stop(f"{model} is no longer listed")
            if priced(pricing):
                return stop(f"{model} is priced now: {json.dumps(priced(pricing))}")
            print(time.strftime("%H:%M"), "free", flush=True)
        time.sleep(every * 60)


if __name__ == "__main__":
    main()
