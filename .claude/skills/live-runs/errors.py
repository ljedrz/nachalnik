"""Every error a tool answered with, across runs, bucketed by its text: errors.py [NAME...]

The row worth reading is a sentence the model did not recover from - the same call again, or the
thing it was doing never done - and a bucket of `_unparsed` arguments, which is the model, the
upstream, or the stream assembly, and only a raw stream through proxy.py says which.
"""
import collections, glob, json, os, re, subprocess, sys

repo = subprocess.check_output(['git', '-C', os.path.dirname(__file__), 'rev-parse', '--show-toplevel'], text=True).strip()
live = os.environ.get('LIVE', os.path.join(repo, 'target/agents/live'))
names = sys.argv[1:] or [os.path.basename(p)[:-5] for p in glob.glob(os.path.join(live, 'out/*.stem'))]
buckets = collections.Counter()
where = collections.defaultdict(set)
for name in names:
    try:
        stem = open(os.path.join(live, 'out', name + '.stem')).read().strip()
        snapshot = json.load(open(stem + '.json'))
    except (OSError, ValueError):
        continue
    for item in snapshot['items']:
        kind = item['kind']
        if kind.get('kind') == 'tool_result' and kind.get('is_error'):
            text = item['content'].get('text', '') if isinstance(item['content'], dict) else ''
            key = re.sub(r'/\S+', 'PATH', re.sub(r'\d+', 'N', text))[:160]
            buckets[key] += 1
            where[key].add(name)
for key, count in buckets.most_common():
    print(f'{count:4}  {key}\n      in {", ".join(sorted(where[key]))}')
