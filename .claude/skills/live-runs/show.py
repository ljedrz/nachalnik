"""The items a run left, from its snapshot: show.py NAME [LABEL-SUBSTRING] [MAX-CHARS]

`shell` for every command's result, `fs:` for every file operation, `context` for the context
tool's answers, and nothing for all of it. The snapshot rather than the prose, since the prose
shows what the model said about a result and this is the result.
"""
import json, os, subprocess, sys

repo = subprocess.check_output(['git', '-C', os.path.dirname(__file__), 'rev-parse', '--show-toplevel'], text=True).strip()
live = os.environ.get('LIVE', os.path.join(repo, 'target/agents/live'))
name = sys.argv[1]
label = sys.argv[2] if len(sys.argv) > 2 else ''
most = int(sys.argv[3]) if len(sys.argv) > 3 else 900
stem = open(os.path.join(live, 'out', name + '.stem')).read().strip()


def text(content):
    if isinstance(content, dict):
        if 'text' in content:
            return content['text']
        if 'blocks' in content:
            return ' | '.join(text(block) for block in content['blocks'])
    return json.dumps(content)[:200]


for item in json.load(open(stem + '.json'))['items']:
    if label and label not in item['label']:
        continue
    print(f"----- [{item['id']}] {item['label']} {item['state']} {item['kind'].get('kind')}")
    print(text(item['content'])[:most])
