//! The reference text a person is shown, kept where both the screen and the commands can reach
//! it.
//!
//! note: not in `ui`, because none of it is drawing: `/help` and `/exclude` print it,
//! and `context` hands the selector list to a *model*. A build with no screen still answers
//! both, so text that a command owns cannot live behind the feature that draws.
//!
//! note: the keys are [`SECTIONS`] rather than one string, and which of them somebody is shown
//! first is decided in `app`, beside the tab that decides it. This module is the words; what
//! applies where is a fact about the program, and `help` has no business knowing that `G` is a
//! trace key any more than the trace has business holding its own sentence about `G`.

/// One page of the key reference: what it is called on the strip, and what it lists.
///
/// note: a page rather than a heading in one long panel, because most of the keys do not apply
/// wherever the panel is opened from. Most sections are about one tab each, and a person on the
/// trace looking for `g` should not have to read past screens of keys that do nothing there. What
/// the strip buys over simply hiding them is that nothing is lost: the sections that do not apply
/// are still one `←` away.
#[derive(Clone, Copy)]
pub struct Section {
    /// What to call it on the strip along the top, in the tab strip's own words where it has one.
    pub name: &'static str,
    /// The keys themselves.
    pub body: &'static str,
    /// Whether this one is about keys, and so about a screen.
    ///
    /// note: stated per section rather than worked out from the name, because the question a
    /// reader of this asks is "have I got keys to press" - and a section added later should have
    /// to answer that rather than inherit an answer from what it happens to be called.
    pub keys: bool,
}

/// Every section, in the order they are offered, which is the order the strip draws them in.
///
/// note: the four tabs first and in the tab strip's own order, so that the two strips on the
/// screen agree about where things are, with `question` beside the chat tab it appears on; then
/// the two that belong to no tab. `question` is a section rather than folded into `chat` because
/// it is only true while a tool is waiting, and [`Section::applies`] leaves it out when nothing
/// is.
pub const SECTIONS: &[Section] = &[
    Section {
        name: "chat",
        body: CHAT,
        keys: true,
    },
    Section {
        name: "question",
        body: QUESTION,
        keys: true,
    },
    Section {
        name: "context",
        body: CONTEXT,
        keys: true,
    },
    Section {
        name: "trace",
        body: TRACE,
        keys: true,
    },
    Section {
        name: "permissions",
        body: PERMISSIONS,
        keys: true,
    },
    Section {
        name: "commands",
        body: COMMANDS,
        // the one page that is not about a screen: a slash command is typed, and every caller this
        // program has can type
        keys: false,
    },
    Section {
        name: "everywhere",
        body: EVERYWHERE,
        keys: true,
    },
];

impl Section {
    /// Whether this one is worth offering at all, to this reader, right now.
    ///
    /// note: two questions and they are about different things. `asked` is about the *session* -
    /// only `question` turns on it, because the keys it lists do not exist until a tool asks for
    /// something. `keys` is about the *reader*: a session driven down a pipe or from a browser has
    /// no keys of this program's to press, and handing it six pages of them is a reference to a
    /// program it is not using. What is left for such a reader is the commands, which everybody
    /// can type.
    ///
    /// note: everything else is offered from everywhere, deliberately: a page somebody has to go
    /// to is not the same as a page that is missing, and a reference that rearranged
    /// itself under them would be one nobody could learn the shape of. The rule above is not that:
    /// a caller with no keys is not somewhere else in the same program, it is somewhere the keys
    /// are not.
    pub fn applies(&self, asked: bool, keys: bool) -> bool {
        if self.keys && !keys {
            return false;
        }

        self.name != "question" || asked
    }
}

/// The whole of it, every section in order, for a reader with no way to turn a page.
///
/// note: every section, `keys` or not, which is why nothing outside the tests reads it: what a
/// caller with no keys is shown for `/help` is decided by [`Section::applies`]. It is kept because
/// a test that checks the words is checking all of them.
pub fn everything() -> String {
    SECTIONS
        .iter()
        .map(|section| section.body)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What the keys do wherever the prompt is, shown by F1 on the chat tab.
///
/// note: no `\` continuation after the opening quote: it would eat the newline *and* the two
/// spaces indenting the heading, leaving it flush against the border while every entry under it
/// sat indented.
pub const CHAT: &str =
    "  THE PROMPT, which is on the chat tab, and wherever an item is being edited
    enter               send
    alt+enter           a new line
    up                  in an empty prompt, the last message back: the one
                        still waiting, if one is, and otherwise a copy of the
                        last one sent. With anything typed it moves the cursor
    down                puts a recalled line away again, while the prompt still
                        says exactly what `up` put there
    pgup / pgdn         scroll the conversation; where you leave it is where
                        it stays, however much arrives underneath
    ctrl+home           the beginning of the conversation
    ctrl+end            the end of it, and following the newest again
    ctrl+e              follow the newest again, from wherever you are
    home / end          the prompt's own, as in any other line editor
    (a message sent while a turn is running waits for the end of it, and
     then gets a turn of its own; `up` is how it is changed or dropped while
     it waits. A turn that stops to ask about a tool has to be answered
     first, because the question is in the prompt's place)";

/// The context tab's own keys.
pub const CONTEXT: &str = "  THE CONTEXT TAB, which has the keys whenever it is open
    up / down, j / k    pick an item
    pgup / pgdn         a screenful at a time
    g / G               the first item / the last
    23G                 the item numbered 23
    space               cycle how much of it the model gets: all of it, then
                        a … marker where it was, then nothing, then all of it
    p                   pin it, so that compaction cannot touch it
                        (on a ▫ archived row, either of those sends the whole
                         of an output the model was shown a truncated copy of)
    e                   change what it says; what it said before is kept
                        (what a turn *did* is not something it says, so a
                         turn that is only a tool call declines the key)
    f                   list only what the next request carries, or everything
    y                   hand the whole of what it says to the terminal, for the
                        clipboard - unwrapped, and with no frame down the sides
                        of it, which is what dragging a mouse over the pane gets
    enter               read the whole of it: what the model gets, what it
                        says, and what it said before it was rewritten
    left / right        move between those, while one is open
    u / U               undo / redo the last change to the context
    /                   filter the rows: fuzzy, over the label, the kind and the
                        whole of what an item holds - so `tool_result` or
                        `assistant` narrows the pane to those - and not only
                        over the line the row has room to show. While the box
                        is open, left and right move within what you have
                        typed, and delete takes out the character in front of
                        the cursor; the keys that move between rows still do
    esc                 clear the filter and close the box";

/// The trace tab's own keys.
pub const TRACE: &str = "  THE TRACE TAB, which has the keys whenever it is open
    up / down, j / k    read back through it
    pgup / pgdn         a screenful at a time
    g / G               the oldest it still holds / the newest
    /                   filter the rows: fuzzy, over the name, the detail and
                        the clock, so an hour or a date finds what happened in
                        it. Reading keys still work while the box is open, and
                        left / right move within the query, so a mistake in the
                        middle of one is a mistake you can go back to
    esc                 clear the filter and close the box";

/// The permissions tab's own keys.
pub const PERMISSIONS: &str = "  THE PERMISSIONS TAB, which has the keys whenever it is open
    up / down, j / k    pick a capability, or one of the path rules under them
    g / G               the first / the last
    space               cycle it: ask, then allow, then deny
    a / n / r           allow it / never allow it / ask about it again
                        (backspace does what r does)
    (the line along the top says which policy is in force and what it answers
     about anything not listed; the one along the bottom says what a shell
     command can reach, and how many subjects are not listed here because
     nobody has answered about them)";

/// The keys that answer a waiting tool, offered only while one is waiting.
pub const QUESTION: &str =
    "  SOMETHING IS WAITING FOR AN ANSWER - in the prompt's place, on the chat
  tab, which goes red on the tab strip while one is there. A tool asking to
  run, or the compaction `/compact` proposed
    tab                 put the keys on it. None of the answers below does
                        anything until you have, and nor does enter
    y / n               once / no
    esc                 no
    up / down, pgup / pgdn   scroll arguments too long for the panel
    a                   always, for everything the question names - and for
                        the calls already waiting behind it
    i                   the exact JSON, and the tool's own definition
    d                   drop every call it is waiting on, and tell it why
    (a compaction takes y, n, esc and the scrolling keys, and nothing else.
     It is holding nothing up: alt+2 to the context tab, p on what should
     stay, back, and answer - the pass is worked out again, so what you kept
     is not in it)
    (it never takes the keys by itself. The answers are bare letters, and a
     question that arrived while somebody was typing once read the `a` of
     `what` as `always, for shell` and kept it for the rest of the session -
     so until you press tab the only keys that do anything are the ones that
     scroll the conversation, which is how you read what the question is
     about before answering it. Whatever was in the prompt is still there
     when the question has gone, with the keys back on it and no second tab
     to press. Coming back to the chat tab from another one also puts them on
     the question, because that is what you came for)";

/// Moving between the tabs, and the keys that mean the same thing on all of them.
///
/// note: one section, rather than one for the tab strip and one for the rest, which is a
/// distinction the reader does not have: both answer "what works no matter where I am", and two
/// headings answering that make the panel look longer than it is.
pub const EVERYWHERE: &str = "  WHEREVER YOU ARE
    ctrl+t              the next tab
    alt+1 / 2 / 3 / 4   chat / context / trace / permissions
    tab                 move the keys between the prompt and whatever else on
                        the screen wants them; from a tab that has no prompt,
                        back to the conversation
    ctrl+p              the exact request that would be sent next
    ctrl+l              take this program's own lines off the chat - what it
                        said about what it did, and what it answered a key
                        with. The conversation stays, because it is the
                        context; the trace keeps what happened either way
    f1                  this, opened at whichever tab you are on; also ? on any
                        tab but the chat one, and ← → for the rest of it
    esc                 close this, or stop what is running
    ctrl+c              stop what is running; again to leave
    ctrl+d              leave";

/// Everything that can be typed at the prompt with a `/` in front of it.
pub const COMMANDS: &str = "  COMMANDS, typed at the prompt on the chat tab
    /help               this; also /?
    /cleanup            take this program's own lines off the chat - what it
                        said about what it did, and what it answered a command
                        with. The conversation stays, because it is the
                        context; the record keeps what happened either way.
                        ctrl+l where there are keys to press
    /step [MESSAGE]     one transition of the state machine, and stop
    /continue           run the rest of the turn
    /stop               stop what is running, keeping whatever arrived. esc and
                        ctrl+c where there are keys to press; typing it is how
                        a client with no keys - a browser - reaches the same act
    /request            the request that would go next
    /payload            the provider's own rendering of it, byte for byte
    /raw                the provider's own last answer
    /attach PATH [TEXT] put a file in the context and ask about it in the same
                        breath. Source and markdown go in as text; a PDF, an
                        image or a recording goes in as itself, which nothing
                        here can price. With no question it just goes in. Not
                        pinned - p does that - where -f at startup is
    /note TEXT          the same with a message instead of a file: something
                        the model should know and does not have to answer.
                        It goes in with the next request rather than starting
                        one, and the model reads it as `note: ...`
    /exclude SELECTOR   take items out of the request; also /prune. With no
                        selector, the whole selector language
    /pin SELECTOR       protect them from compaction; also /keep
    /restore SELECTOR   put them back
    /budget             the estimate, what the last request really cost, and the
                        correction the counter has worked out from the difference
    /copy [N]           hand the last thing the model said to the terminal, for
                        the clipboard, or item N. `y` on the context tab is the
                        same act on the row it is standing on
    /compact            what the compactor would take, listed, and then `y` or
                        `n`. The list waits, so `p` on the context tab keeps
                        something out of it and the pass is worked out again
    /spend [TOKENS]     what the provider has charged for this session, and the
                        ceiling it stops at; 0 takes the ceiling away
    /seams              what is plugged into each of the runtime's six parts
    /tools              what the model is offered, and what is turned off
    /tools toggle ID    stop offering one of them, or offer it again. The one
                        that reads this session's context and the one that
                        changes it are tools like any other
    /limit              how much of a call's output the model is shown, by
                        subject, numbered, and the number is one the next
                        line takes
    /limit SUBJECT BYTES
                        change one, from its next call onwards. A subject is
                        what a permission is: fs:read, exec:run
    /policy             open the permissions tab; also /permissions
    /model [ID]         show or switch the model, and say where it is
    /models [FILTER]    what this endpoint serves, which is what /model takes
    /provider [URL [ID]] show or switch the address the requests go to, and
                        the model with it; also /endpoint. The key is the one
                        this started with
    /params [KEY JSON]  show or set a model parameter, and what else this model
                        takes. One it does not take is sent and ignored
    /save [PATH]        the session log, and a snapshot to resume from
    /load [PATH]        that snapshot's context, into the session you are in.
                        What is here is archived unless it is pinned, and u
                        twice puts it back (kamchatka -r PATH is the other
                        answer to the same file: a fresh session from it)
    /restart            write this session out and start a fresh one, as if the
                        program had just been run: a new log, an empty context,
                        and the flags back the way they were. Stops a running
                        turn to do it
    /quit               also /exit, /q";

/// The selector language, shown by `/exclude` with nothing to exclude.
///
/// note: Kept beside the help rather than derived from the crate, because `Selector` is a parser
/// and a parser cannot tell you what it would have accepted. It is the same list as the type's
/// own documentation, and the tests check that a few of these really do parse.
pub(crate) const SELECTORS: &str = "  17                      the item with that number
  all                     every item, whatever state it is in

  tool_results            every item from that source; also: files, diagnostics,
                          selections, memories, instructions, system, user, model,
                          compaction
  all:tool_results        the same, spelled out
  source:helix            every item from a source with that name

  kind:assistant_message  every item of that kind
  state:excluded          every item in that state

  file:src/parser.rs      the file with that path
  tool:grep               every result the `grep` tool produced
  tool:grep:latest        the most recent one; also: tool:grep:first
  tool_result:1842        the item numbered 1842, the same as `1842`
  label:cargo test        every item with exactly that label
  src/parser.rs           anything else is taken as a label

  What it matched is reported before anything is sent, and every change is one
  `u` away from being undone.";
