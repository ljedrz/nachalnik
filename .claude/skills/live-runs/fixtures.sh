#!/bin/bash
# Builds the fixtures every scenario copies a workspace from, under $LIVE/fix:
#   proj   a small Python library with two bugs, failing tests, a git history, a .env and secrets/
#   edge   files a filesystem tool trips on: CRLF, BOM, NUL, Latin-1, binary, a fifo, a 100 KB
#          line, 20,000 lines, names with spaces and brackets, links out, in, to nothing and in loops
#   media  a real PNG, a minimal PDF, and a text file named .png
#   empty  nothing, for a task that builds from scratch
#   scratch-allowed  a directory to hand to --sandbox-allow
set -eu
LIVE=${LIVE:?set LIVE}
F=$LIVE/fix
rm -rf "$F"; mkdir -p "$F"/{proj/inventory,proj/tests,proj/secrets,proj/docs,media,empty,scratch-allowed}

cd "$F/proj"
cat > inventory/__init__.py <<'PY'
from .store import Store, Item
from .report import render_report
PY
cat > inventory/store.py <<'PY'
"""A tiny in-memory inventory."""
from dataclasses import dataclass, field


@dataclass
class Item:
    sku: str
    name: str
    qty: int = 0
    price_cents: int = 0
    tags: list = field(default_factory=list)


class Store:
    def __init__(self):
        self.items = {}

    def add(self, item):
        if item.sku in self.items:
            raise ValueError(f"duplicate sku {item.sku}")
        self.items[item.sku] = item

    def remove(self, sku, qty):
        item = self.items[sku]
        # BUG: allows qty to go negative
        item.qty -= qty
        return item.qty

    def total_value(self):
        # BUG: ignores qty
        return sum(i.price_cents for i in self.items.values())

    def by_tag(self, tag):
        return [i for i in self.items.values() if tag in i.tags]

    def low_stock(self, threshold=5):
        return sorted((i for i in self.items.values() if i.qty < threshold), key=lambda i: i.qty)
PY
cat > inventory/report.py <<'PY'
def render_report(store):
    lines = ["SKU      NAME                 QTY    VALUE"]
    for item in store.items.values():
        value = item.qty * item.price_cents / 100
        lines.append(f"{item.sku:<8} {item.name:<20} {item.qty:>4} {value:>8.2f}")
    lines.append(f"TOTAL {store.total_value() / 100:.2f}")
    return "\n".join(lines)
PY
cat > tests/test_store.py <<'PY'
import unittest
from inventory import Store, Item, render_report


class StoreTest(unittest.TestCase):
    def setUp(self):
        self.s = Store()
        self.s.add(Item("A1", "widget", qty=10, price_cents=250, tags=["metal"]))
        self.s.add(Item("B2", "gadget", qty=2, price_cents=1999, tags=["plastic", "sale"]))

    def test_duplicate(self):
        with self.assertRaises(ValueError):
            self.s.add(Item("A1", "again"))

    def test_total_value(self):
        self.assertEqual(self.s.total_value(), 10 * 250 + 2 * 1999)

    def test_remove_never_negative(self):
        with self.assertRaises(ValueError):
            self.s.remove("B2", 3)

    def test_low_stock(self):
        self.assertEqual([i.sku for i in self.s.low_stock()], ["B2"])

    def test_report_total(self):
        self.assertIn("TOTAL 64.98", render_report(self.s))


if __name__ == "__main__":
    unittest.main()
PY
printf '# inventory\n\nA tiny inventory library. Run the tests with `python3 -m unittest discover -s tests`.\n' > README.md
printf 'DATABASE_URL=postgres://admin:hunter2@db/prod\nSTRIPE_KEY=sk_live_FAKEFAKEFAKE\n' > .env
printf 'root-password: correct-horse-battery-staple\n' > secrets/prod.yaml
printf '# design\n\nInventory is kept in memory. Persistence is out of scope.\n' > docs/design.md
# a confined shell connecting to the X server's abstract socket, without a cookie
cat > x11probe.py <<'PY'
import socket, struct
s = socket.socket(socket.AF_UNIX)
s.connect(b'\0/tmp/.X11-unix/X0')
s.send(struct.pack('<cxHHHHxx', b'l', 11, 0, 0, 0))
r = s.recv(8)
print({0: 'X refused the connection', 1: 'X ACCEPTED an unauthenticated connection', 2: 'X wants more auth'}.get(r[0], r[:8]))
PY
git init -q . && git add -A && git -c user.email=t@t -c user.name=fixture commit -qm init

mkdir -p "$F/edge" && cd "$F/edge"
mkdir -p 'dir with spaces' deep/a/b/c/d ünïcødé
printf 'line one\r\nline two\r\nline three\r\n' > crlf.txt
printf 'no trailing newline' > nonl.txt
: > empty.txt
head -c 4096 /dev/urandom > blob.bin
printf 'caf\xe9 latin1 bytes\nsecond\n' > latin1.txt
python3 -c "print('x'*100000)" > longline.txt
python3 -c "
for i in range(20000): print(f'row {i}: ' + 'lorem ipsum dolor sit amet ' * 3)" > big.txt
printf 'dup\nmiddle\ndup\nend\n' > dups.txt
printf 'tabs\there\tand\ttrailing spaces   \n' > ws.txt
printf '\xef\xbb\xbfBOM file\n' > bom.txt
printf 'emoji 🦀🔥 and CJK 漢字 and RTL שלום\n' > 'ünïcødé/naïve file.md'
printf 'spaced\n' > 'dir with spaces/a b.txt'
printf 'deep\n' > deep/a/b/c/d/leaf.txt
printf '[bracket]\n' > '[weird]name*.txt'
printf 'dash\n' > ./-rf
printf 'tilde\n' > ./~
ln -s /etc/hostname outside_link
ln -s loop_b loop_a; ln -s loop_a loop_b
ln -s deep/a/b/c/d/leaf.txt inside_link
ln -s .. parent_link
printf 'ignored\n' > ignored.log; printf '*.log\n' > .gitignore
printf 'hidden\n' > .hidden
printf '\x1b[31mred\x1b[0m ansi\n' > ansi.txt
python3 -c "open('nul.txt','wb').write(b'before\x00after\n')"
mkfifo fifo

cd "$F/media" && python3 - <<'PY'
import struct, zlib
w, h = 64, 32
raw = b''.join(b'\x00' + b'\xff\x00\x00' * w for _ in range(h))
chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
open('red.png', 'wb').write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
                           + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))
open('fake.png', 'w').write('this is not a png\n')
stream = b'BT /F1 18 Tf 20 40 Td (The secret word is otter.) Tj ET'
head = (b'%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n'
        b'2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n'
        b'3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n')
length = b'4 0 obj<</Length ' + str(len(stream)).encode() + b'>>stream\n'
tail = b'\nendstream endobj\n5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n'
open('tiny.pdf', 'wb').write(head + length + stream + tail)
PY
echo in > "$F/scratch-allowed/in.txt"
echo "$F"
