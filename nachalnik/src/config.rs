#[cfg(doc)]
use crate::{Compactor, Event, Kernel, ToolSpec};

/// The kernel's configuration. See the source of [`Config::default`] for the defaults.
///
/// note: There is no field here that changes *what the model is told*. The kernel ships no
/// system prompt, no instructions and no message templates; all of that is context the user
/// supplies (see [`Kernel::push`]).
#[derive(Debug, Clone)]
pub struct Config {
    /// A user-friendly identifier of the session. It shows up in [`Event::SessionStarted`] and
    /// in exported session logs, where it allows sessions to be told apart.
    ///
    /// note: If set to `None`, the kernel will automatically be assigned a sequential,
    /// zero-based numeric identifier.
    pub session_name: Option<String>,
    /// The capacity of the event broadcast channel.
    ///
    /// note: A subscriber that falls this far behind starts losing events; the session log
    /// ([`Kernel::history`]) is the authoritative record, the event stream is the live one.
    pub event_queue_depth: usize,
    /// Whether [`Event::ModelDelta`] and [`Event::ToolOutput`] events are appended to the
    /// session log.
    ///
    /// note: They are always broadcast to subscribers; this only decides whether streaming
    /// fragments are also *kept*, which is rarely useful (the assembled result is recorded
    /// either way) and can dominate the log.
    pub record_progress: bool,
    /// How many context snapshots are retained for [`Kernel::undo`]; `0` disables undo.
    pub context_undo_depth: usize,
    /// The maximum number of requests a single [`Kernel::turn`] may send before it hands
    /// control back; `None` means it keeps going until the model stops asking for tools.
    ///
    /// note: This is a stop, not a policy: reaching it just ends the turn, and calling
    /// [`Kernel::turn`] again resumes exactly where it left off.
    pub max_requests_per_turn: Option<usize>,
    /// The default limit (in bytes) applied to tool output, used for tools whose
    /// [`ToolSpec::output_limit`] is `None`; `None` means tool output is never truncated by
    /// the kernel.
    ///
    /// note: Truncation is recorded on the context item and reported in
    /// [`Event::ToolFinished`], so it can never happen silently.
    pub default_tool_output_limit: Option<usize>,
    /// Whether the whole of a truncated tool output is kept in the context, archived beside the
    /// shortened copy the model is shown.
    ///
    /// note: On by default, because an output limit is a decision about what the *model* is
    /// shown, and reading it as permission to destroy what the tool actually said would make
    /// this the one place in the crate where something is removed and cannot be brought back.
    /// With it on, the whole output is a
    /// [`ContextState::Archived`](crate::ContextState::Archived) item, listed and inspectable
    /// like any other, and restoring it is a [`Kernel::set_state`] like any other.
    ///
    /// note: Keeping it costs a pointer, not a copy - the archived item shares the allocation
    /// the tool already produced, and only the shortened copy is new. What it does cost is
    /// *retention*: with this off, the whole output is dropped once it has been shortened. Turn
    /// it off when a tool can produce more than you are willing to go on holding. The truncation
    /// is still reported either way; what changes is whether it is reversible.
    pub keep_truncated_output: bool,
    /// Whether the payload a [`Provider`](crate::Provider) renders for each request is broadcast
    /// and recorded, as [`Event::ModelPayload`].
    ///
    /// note: Off by default because of *memory*, which is the cost that cannot be worked around.
    /// A stateless chat API is re-sent the whole conversation every time, so payload n contains
    /// payload n-1: recording them all is quadratic in the number of requests, and the log is a
    /// live structure in RAM that nothing can compress. Twenty-four turns of a small
    /// conversation come to 1.3 MB of payloads against 71 KB without them.
    ///
    /// note: On *disk* it is much cheaper than that sounds, because a log whose entries repeat
    /// each other's prefixes is the best case there is for an ordinary compressor - the same
    /// 1.3 MB is 4.8 KB under `xz`, against 3.5 KB for the log without payloads. If you want
    /// them, the pattern is [`Kernel::drain_history`] on a schedule: take the records, compress
    /// them somewhere, and stop paying for them in memory.
    ///
    /// note: With it off, [`Kernel::preview_payload`] still renders on demand, so nothing is
    /// hidden - it is only not kept.
    pub record_payloads: bool,
    /// Whether the tools a model asks for in one turn are run at the same time.
    ///
    /// note: Off by default, and not because of performance. With it off the calls run one at a
    /// time, in the order the model asked for them, and that order is something you can build
    /// on: two edits to the same file apply in sequence. Turning it on gives that up. Nothing in
    /// the kernel can tell whether a model's calls are independent of each other and nothing in a
    /// [`Tool`](crate::Tool) declares it, so the judgement is yours and it has to be made on
    /// purpose.
    ///
    /// note: What does *not* change is the context. Results are recorded in the order the model
    /// asked for them, once they have all finished, so a session looks the same whichever mode it
    /// ran in. What varies is the order of [`Event::ToolStarted`] and [`Event::ToolOutput`],
    /// which now genuinely do interleave - and the fact that you see no
    /// [`Event::ToolFinished`] until the slowest call is done.
    ///
    /// note: This is the one place the kernel spawns tasks, and only for the length of the step
    /// that spawned them. Dropping the future driving [`Kernel::step`] aborts them, so a
    /// cancelled turn is cancelled here too.
    pub parallel_tool_calls: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session_name: None,
            event_queue_depth: 1024,
            record_progress: false,
            context_undo_depth: 16,
            max_requests_per_turn: Some(8),
            default_tool_output_limit: None,
            keep_truncated_output: true,
            record_payloads: false,
            parallel_tool_calls: false,
        }
    }
}
