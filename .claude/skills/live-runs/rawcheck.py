"""Calls whose streamed arguments do not parse, in the logs proxy.py wrote: rawcheck.py LOGPREFIX

For each, the file, the call's index, what its arguments came to, and how the stream ended - a
`finish_reason`, or an `error` event the endpoint sent mid-answer. A stream that says `tool_calls`
over arguments cut short is the upstream's; one that ends in an error event is a failure the
client should have reported as one; and arguments that parse here and not in the record are this
workspace's own assembly.
"""
import glob, json, sys

for path in sorted(glob.glob(sys.argv[1] + '.*')):
    raw = open(path, 'rb').read().decode('utf-8', 'replace')
    if 'RESPONSE' not in raw:
        continue
    args, ended = {}, None
    for line in raw.split('RESPONSE', 1)[1].splitlines():
        if not line.startswith('data: {'):
            continue
        event = json.loads(line[6:])
        if event.get('error'):
            ended = 'error event: ' + json.dumps(event['error'])[:120]
        for choice in event.get('choices') or []:
            if not (ended or '').startswith('error event'):
                ended = choice.get('finish_reason') or ended
            for call in (choice.get('delta') or {}).get('tool_calls') or []:
                written = (call.get('function') or {}).get('arguments') or ''
                args[call.get('index')] = args.get(call.get('index'), '') + written
    for index, written in args.items():
        try:
            json.loads(written or '{}')
        except ValueError:
            print(f'{path}  call {index}: {written[:80]!r}  ended: {ended}')
