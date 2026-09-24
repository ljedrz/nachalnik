#!/usr/bin/env python3
"""What a finished sweep said: its replies, with -r its reasoning, with -n its notes.

usage: extract.py SNAPSHOT [-r] [-n]   (SNAPSHOT from snap.sh NAME)
"""
import json,sys
d=json.load(open(sys.argv[1]))
full='-r' in sys.argv
notes='-n' in sys.argv
def text(c):
    if c is None: return ''
    if isinstance(c,str): return c
    if isinstance(c,dict):
        for k in ('text','Text'):
            if k in c and isinstance(c[k],str): return c[k]
        if 'blocks' in c: return '\n'.join(text(b) for b in c['blocks'])
        return json.dumps(c)[:2000]
    if isinstance(c,list): return '\n'.join(text(x) for x in c)
    return str(c)
for i in d['items']:
    k=i['kind']
    if k.get('kind')!='assistant_message': continue
    t=text(i.get('content'))
    r=text(k.get('reasoning'))
    if t.strip():
        print(f'=== item {i["id"]} TEXT ===\n{t}\n')
    if full and r.strip():
        print(f'--- item {i["id"]} reasoning ---\n{r}\n')

# the notes a sweep wrote with the `context` tool, which is where a context-hygiene run keeps its
# findings: items whose source is `agent`
if notes:
    for i in d['items']:
        if i.get('source')=='agent':
            print(f'=== note {i["id"]} [{i.get("label","")}] ({i.get("state","")}) ===\n{text(i.get("content"))}\n')
