//! The two pieces of reference text a person is shown, kept where both the screen and the
//! commands can reach them.
//!
//! note: not in `ui`, where they were, because neither of them is drawing: `/help` and `/prune`
//! print them, and the `amend` tool hands the selector list to a *model*. A build with no screen
//! still answers both, so text that a command owns cannot live behind the feature that draws.

/// What the keys do, shown by F1.
///
/// note: `pub` so that a test can read it rather than trying to count things on a screen it does
/// not all fit on. That is not a hypothetical convenience: `/seams` was listed in here twice, and
/// the test that draws this panel had no way to notice.
///
/// note: no `\` continuation after the opening quote: it would eat the newline *and* the two
/// spaces indenting the first heading, leaving `THE TABS` flush against the border while every
/// other heading sat under it.
pub const HELP: &str = "  THE TABS
    ctrl+t              the next one
    alt+1 / 2 / 3 / 4   chat / context / trace / permissions
    tab                 move the keys between the prompt and whatever else on
                        the screen wants them; from a tab that has no prompt,
                        back to the conversation

  ANYWHERE
    ctrl+p              the exact request that would be sent next
    f1                  this; also ? on any tab but the chat one
    esc                 close this, or stop what is running
    ctrl+c              stop what is running; again to leave
    ctrl+d              leave

  THE PROMPT, which is on the chat tab, and wherever an item is being edited
    enter               send
    alt+enter           a new line
    pgup / pgdn         scroll the conversation; where you leave it is where
                        it stays, however much arrives underneath
    ctrl+home           the beginning of the conversation
    ctrl+end            the end of it, and following the newest again
    ctrl+e              follow the newest again, from wherever you are
    home / end          the prompt's own, as in any other line editor
    (a message sent while a turn is running waits for the end of it, and
     then gets a turn of its own; a turn that stops to ask about a tool has
     to be answered first, because the question is in the prompt's place)

  THE CONTEXT TAB, which has the keys whenever it is open
    up / down, j / k    pick an item
    pgup / pgdn         a screenful at a time
    g / G               the first item / the last
    23G                 the item numbered 23
    space               cycle how much of it the model gets: all of it, then
                        a … marker where it was, then nothing, then all of it
    p                   pin it, so that compaction cannot touch it
                        (on a ▫ archived row, either of those sends the whole
                         of an output the model was shown a truncated copy of)
    e                   change what it says; the old one stays, marked ~
    f                   list only what the next request carries, or everything
    enter               read the whole of it: what the model gets, what it
                        says, and what it said before it was rewritten
    left / right        move between those, while one is open
    u / U               undo / redo the last change to the context

  THE TRACE TAB, which has the keys whenever it is open
    up / down, j / k    read back through it
    pgup / pgdn         a screenful at a time
    g / G               the oldest it still holds / the newest

  THE PERMISSIONS TAB, which has the keys whenever it is open
    up / down, j / k    pick a capability, or one of the path rules under them
    g / G               the first / the last
    space               cycle it: ask, then allow, then deny
    a / n / r           allow it / never allow it / ask about it again
                        (backspace does what r does)
    (the line along the top says which policy is in force and what it answers
     about anything not listed; the one along the bottom says what a shell
     command can reach, and how many subjects are not listed here because
     nobody has answered about them)

  A TOOL IS WAITING TO RUN - in the prompt's place, on the chat tab, which
  goes red on the tab strip while one is there
    tab                 put the keys on it. None of the answers below does
                        anything until you have, and nor does enter
    y / n               once / no
    esc                 no
    up / down, pgup / pgdn   scroll arguments too long for the panel
    a                   always, for everything the question names - and for
                        the calls already waiting behind it
    i                   the exact JSON, and the tool's own definition
    d                   drop every call it is waiting on, and tell it why
    (it never takes the keys by itself. The answers are bare letters, and a
     question that arrived while somebody was typing once read the `a` of
     `what` as `always, for shell` and kept it for the rest of the session -
     so until you press tab the only keys that do anything are the ones that
     scroll the conversation, which is how you read what the question is
     about before answering it. Whatever was in the prompt is still there
     when the question has gone, with the keys back on it and no second tab
     to press. Coming back to the chat tab from another one also puts them on
     the question, because that is what you came for)

  COMMANDS
    /help               this; also /?
    /step [MESSAGE]     one transition of the state machine, and stop
    /continue           run the rest of the turn
    /request            the request that would go next
    /payload            the provider's own rendering of it, byte for byte
    /raw                the provider's own last answer
    /attach PATH [TEXT] put a file in the context and ask about it in the same
                        breath. Source and markdown go in as text; a PDF, an
                        image or a recording goes in as itself, which nothing
                        here can price. With no question it just goes in. Not
                        pinned - p does that - where -f at startup is
    /exclude SELECTOR   take items out of the request; also /prune. With no
                        selector, the whole selector language
    /pin SELECTOR       protect them from compaction; also /keep
    /restore SELECTOR   put them back
    /budget             the estimate, what the last request really cost, and the
                        correction the counter has worked out from the difference
    /spend [TOKENS]     what the provider has charged for this session, and the
                        ceiling it stops at; 0 takes the ceiling away
    /seams              what is plugged into each of the runtime's six parts
    /tools              what the model is offered
    /tools drop ID      stop offering one of them, from now on
    /limit              how much of each tool's output the model is shown,
                        numbered, and the number is one the next line takes
    /limit ID BYTES     change one, from its next call onwards
    /introspect         offer the model the two tools that read and manage its own
                        context, or stop offering them
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
    /quit               also /exit, /q";

/// The selector language, shown by `/prune` with nothing to prune.
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
  tool_result:1842        the tool result with that call id
  label:cargo test        every item with exactly that label
  src/parser.rs           anything else is taken as a label

  What it matched is reported before anything is sent, and every change is one
  `u` away from being undone.";
