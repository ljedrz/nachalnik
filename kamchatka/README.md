# kamchatka

**A terminal agent that shows you its context.**

Built on [`nachalnik`](../nachalnik), and built to demonstrate it. Everything in here is
ordinary user code — the provider, the four tools, the permission policy, the compactor, the
drawing. The runtime supplies the state machine, the context and the paper trail.

```console
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder -f src/lib.rs "what does this crate do?"
```

```text
┌ conversation ──────────────────────────────────────────────────────┐┌ context │ trace ───────────────────────┐
│· [1] src/kernel.rs is in the context, 1000 tokens                  ││  1 ▪ src/kernel.rs                1,000│
│                                                                    ││  2 · user                             6│
│⟩ read({"path":"src/kernel.rs"})                                    ││  3 · assistant                        7│
│                                                                    ││  4 - read                            15│
││ pub struct Kernel(Arc<InnerKernel>);                              ││      excluded: pruned at the terminal  │
│  // ... 900 more lines                                             ││  5 · assistant                        7│
│                                                                    ││  6 · shell                            7│
│· read: 15 tokens                                                   ││  7 · assistant                       43│
│                                                                    ││                                        │
│⟩ shell({"cmd":"cargo test"})                                       ││                                        │
│                                                                    ││                                        │
││ test result: ok. 173 passed                                       ││                                        │
│                                                                    ││                                        │
│· shell: 7 tokens                                                   ││                                        │
│                                                                    ││                                        │
│The kernel is a state machine with five states. `step` performs one ││                                        │
│transition and returns the state it produced; `turn` repeats it     ││                                        │
│until the model stops asking for tools.                             ││                                        │
└────────────────────────────────────────────────────────────────────┘└──────────── 7 items · ~1,101 / 128,000 ┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · gpt-4o-mini · ~1,101 tokens, 0.9% of the limit · 1,043 really · 15 held back · F1 for the keys
```

## 👉 the pane on the right

Every terminal agent has the conversation. The **context** beside it is why this one exists.

It is not a summary and not a debug view: it is the list of items the runtime is holding, in
order, with what each one costs and whether it is going into the next request. Item 4 in that
screenshot is marked `-` because somebody took it out, and says underneath itself why — which is
also why the status line reads `15 held back`. Item 1 is `▪`, pinned, so the compactor will be
refused if it comes for it. Nothing disappeared: things changed state, and the state is on screen.

Press <kbd>tab</kbd> to move over to it:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | take an item out of the next request, or put it back |
| <kbd>p</kbd> | pin it, so that the compactor is refused if it tries |
| <kbd>enter</kbd> | read what the item actually says |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context |

An item that is not going into the next request says so underneath itself, in the projector's own
words — `excluded: pruned at the terminal`, `archived: the whole output; the model was shown a
shortened copy`. "Why is that out?" is a question about the thing you are looking at.

## 🧾 the other tab

<kbd>ctrl+t</kbd> swaps the pane for the trace: every event the runtime emits, as it happens, in
the same names the session log is made of.

```text
┌ conversation ──────────────────────────────────────────────────────┐┌ context │ trace ───────────────────────┐
│                                                                    ││model.requested                         │
│                                                                    ││  4 messages, 2 tools, ~1066 tokens     │
│                                                                    ││context.added  [5] assistant, 7 tokens  │
│                                                                    ││model.finished  ToolUse                 │
│                                                                    ││tool.requested  shell                   │
│                                                                    ││permission.decided                      │
│                                                                    ││  shell: allow, by the User             │
│                                                                    ││state.changed  requesting → ready       │
│                                                                    ││state.changed  ready → executing        │
│                                                                    ││tool.started  shell                     │
│                                                                    ││tool.output  shell, 27 bytes            │
│                                                                    ││context.added  [6] shell, 7 tokens      │
│                                                                    ││tool.finished  shell, 7 tokens          │
│                                                                    ││state.changed  executing → idle         │
│                                                                    ││model.requested                         │
│                                                                    ││  6 messages, 2 tools, ~1080 tokens     │
│                                                                    ││context.added  [7] assistant, 43 tokens │
│                                                                    ││model.finished  EndTurn                 │
│                                                                    ││state.changed  requesting → finished    │
└────────────────────────────────────────────────────────────────────┘└────── 34 events · /save keeps them all ┘
```

It is the same stream `/save` writes to a `.jsonl`, and reading it is how you find out that a
permission question became a decision became a state change became a call. <kbd>tab</kbd> then
<kbd>up</kbd> reads back through it.


And <kbd>ctrl+p</kbd> prints the request those items add up to — the kernel's own rendering of it,
not a description, with a header naming everything the projector left out and why:

```text
12 item(s) in, 4 out:
  [13] left out: an assistant turn with no content and no answered calls
  [14] left out: archived: the whole output; the model was shown a shortened copy
  [15] left out: excluded: pruned by `tool:shell:latest`
  repaired: dropped the call `call_301842` (shell) from item 13: its result is not
            in the projection
```

"why is that not in there?" is the question this whole runtime is for, and the JSON on its own can
only answer the other one. `/payload` goes one further and prints what the provider will put on
the wire, byte for byte.

## ⌨️ the rest of the keys

| key | what happens |
| --- | --- |
| <kbd>enter</kbd> / <kbd>alt+enter</kbd> | send / a new line |
| <kbd>tab</kbd> | move between the prompt and the pane beside it |
| <kbd>ctrl+t</kbd> | swap the pane between the context and the trace |
| <kbd>esc</kbd> | stop what is running, and keep what arrived |
| <kbd>ctrl+c</kbd> | the same, and again to leave |
| <kbd>F1</kbd> | all of them, including the slash commands |

Stopping is cooperative rather than a killed process: the provider notices between fragments and
returns the text it has, the shell tool kills its child and still answers the call it was given,
and the partial turn ends up in the context like any other — where it can be read, pruned, or left
alone.

## 🔧 what it comes with

Four tools — `read`, `write`, `edit`, `shell` — and a policy that allows reading, refuses the
network, and asks about everything else. Answering **always** answers for a *capability*, not a
tool name, which is what makes it work for tools this program has never heard of:

```console
$ kamchatka --mcp 'files=npx -y @modelcontextprotocol/server-filesystem /srv'
```

Those arrive through [`nachalnik-mcp`](../nachalnik-mcp) carrying `mcp:files`, so "always, for
mcp:files" is one server and not the next one. The `name=` is worth giving: it prefixes the
server's tools and it is what the grant is *for*, and without it the name comes from the program,
which for most of the servers people actually run is `npx`.

## 📏 the number in the status line is a guess, and says so

Nothing here has the model's tokenizer, so the figure the status line leads with is an estimate —
it is written `~2,460` for that reason. Beside it is what the provider actually charged for the
last request, and `/budget` is where the two are reconciled:

```text
the next request: ~20,125 tokens, 19,953 of context and 172 of tool definitions

the limit: 1,048,576, which the next request would fill 1.9% of

the last request really cost 20,063, as the provider counted it

the counter has learned from 2 request(s) and scaled itself by 1.131: its own guesses
came to 35,447 tokens where the provider counted 40,073, so it was reading 13.1% low
```

That correction is the runtime's `Calibrating` counter: every response tells it what the request
it just estimated really cost, and it adjusts. Over a real session against Gemini it went from 13%
low to within 0.3%. A budget nobody can check is a decoration.

The compactor drops the oldest tool results once the context passes `--compact` (0.8 by default)
of the limit and leaves a note saying which ones went. It does not summarize them — it never read
them — and it removes nothing that is pinned, because the kernel refuses. Every removal is an
excluded item you can put back.

## 🎛️ options

```text
kamchatka [OPTIONS] [MESSAGE]...

  -m, --model <MODEL>       the model to talk to            [env: KAMCHATKA_MODEL]
  -f, --file <PATH>         put a file in the context, pinned; may be repeated
  -s, --system <TEXT>       a system instruction; the runtime ships none of its own
  -r, --resume <PATH>       carry on from a session written by /save
      --mcp <COMMAND>       an MCP server to run, as `[name=]command`; may be repeated
      --requests <N>        how many requests one turn may make            [default: 8]
      --compact <FRACTION>  how full the context may get; 1 never compacts [default: 0.8]
      --parallel            run the model's tool calls at the same time
```

```text
KAMCHATKA_API_KEY   or OPENROUTER_API_KEY, or OPENAI_API_KEY
KAMCHATKA_BASE_URL  e.g. http://localhost:11434/v1 for ollama; OpenRouter by default
```

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot that
`-r` picks the session back up from.

## 🧪 the tests

They draw the screen and read it back, against a scripted model:

```rust
harness.press(KeyCode::Tab).await;       // over to the context
harness.press(KeyCode::Home).await;      // the first item
harness.press(KeyCode::Char(' ')).await; // out

let after = harness.app.kernel.preview_request().unwrap();
assert!(!format!("{:?}", after.messages).contains("hunter2"));
```

A terminal program whose tests only checked its own state would be testing the half nobody looks
at.

## 🎸 the name

`nachalnik` is an homage to KINO's *Nachalnik Kamchatki*. Kamchatka was the boiler room Viktor
Tsoi shovelled coal in; this is the one where the work actually happens.

## licence

MIT.
