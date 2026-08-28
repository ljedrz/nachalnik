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
┌ conversation ──────────────────────────────────────────────┐┌ context · 140 / 128,000 ───────────┐
│· tab moves to the context · F1 lists the keys · ctrl+p     ││  1 - src/kernel.rs            1,000│
│  shows the next request                                    ││  2 · user                         6│
│                                                            ││  3 · assistant                    7│
│· [1] src/kernel.rs is in the context, 1000 tokens          ││  4 · read                        15│
│                                                            ││  5 · assistant                   74│
│⟩ read({"path":"src/kernel.rs"})                            ││                                    │
│                                                            ││                                    │
││ pub struct Kernel(Arc<InnerKernel>);                      ││                                    │
│  // ... 900 more lines                                     ││                                    │
│                                                            ││                                    │
│· read: 15 tokens                                           ││                                    │
│                                                            ││                                    │
│The kernel is a state machine with five states. `step`      ││                                    │
│performs one transition and returns the state it produced;  ││                                    │
│`turn` repeats it until the model stops asking for tools.   │└────────────────────────────────────┘
│Nothing in it decides what the model is told - that is the  │┌ trace ─────────────────────────────┐
│projector's job, and the projector is a trait you can       ││context.added  [4] read, 15 tokens  │
│replace.                                                    ││tool.finished  read, 15 tokens      │
│                                                            ││state.changed  executing → idle     │
│                                                            ││state.changed  idle → requesting    │
│                                                            ││model.requested  4 messages, 2 tool…│
│                                                            ││context.added  [5] assistant, 74 to…│
│                                                            ││model.finished  EndTurn             │
│                                                            ││state.changed  requesting → finished│
└────────────────────────────────────────────────────────────┘└────────────────────────────────────┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                      │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · scripted · 140 tokens, 0.1% of the limit · 1,000 held back · F1 for the keys
```

## 👉 the pane on the right

Every terminal agent has the conversation. The **context** beside it is why this one exists.

It is not a summary and not a debug view: it is the list of items the runtime is holding, in
order, with what each one costs and whether it is going into the next request. `src/kernel.rs` in
that screenshot is marked `-` because somebody took it out — which is why the status line says
`1,000 held back`, and why the trace says `skipped [1]: excluded: too big` the next time a request
goes out. Nothing disappeared; it changed state, and the state is on screen.

Press <kbd>tab</kbd> to move over to it:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | take an item out of the next request, or put it back |
| <kbd>p</kbd> | pin it, so that the compactor is refused if it tries |
| <kbd>enter</kbd> | read what the item actually says |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context |

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
| <kbd>tab</kbd> | move between the prompt and the context |
| <kbd>ctrl+t</kbd> | show or hide the trace — every event, as it happens |
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
