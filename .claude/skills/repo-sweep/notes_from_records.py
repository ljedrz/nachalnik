#!/usr/bin/env python3
"""The notes a sweep wrote, read out of its stream records rather than its snapshot.

For a sweep that was killed before it could write a snapshot: every `context note` call is a
record, with the note in its arguments.

usage: notes_from_records.py $SWEEPS/NAME.jsonl
"""
import json,sys
for l in open(sys.argv[1]):
    try: e=json.loads(l)['event']
    except: continue
    if e.get('event')!='tool.requested' or e.get('tool')!='context': continue
    a=e.get('args') or {}
    if isinstance(a,dict) and 'call' in a: a=a['call']
    if isinstance(a,dict) and a.get('action')=='note':
        rest={k:v for k,v in a.items() if k!='action'}
        t=rest.pop('text',None) or rest.pop('content',None) or rest.pop('note',None)
        print('=== note',json.dumps(rest)[:200]); print(t)
