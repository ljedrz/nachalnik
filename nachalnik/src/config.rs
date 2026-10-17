//! The knobs, with the reasoning beside each default that needs it.

#[cfg(doc)]
use crate::{Event, Kernel, TokenCounter, ToolSpec};

/// The kernel's configuration. See the source of [`Config::default`] for the defaults.
///
/// note: There is no field here that changes *what the model is told*: they decide how the loop
/// behaves and how much of what it does is recorded. The kernel ships no system prompt, no
/// instructions and no message templates; everything the model reads is context somebody pushed
/// (see [`Kernel::push`]).
#[derive(Debug, Clone)]
pub struct Config {
    /// A user-friendly identifier of the session. It appears in [`Event::SessionStarted`] and
    /// exported session logs, where it distinguishes sessions.
    ///
    /// note: If set to `None`, the session is given a sequential, zero-based numeric identifier,
    /// counted across every kernel in the process.
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
    /// The maximum number of requests a turn may send before it hands control back; `None` means it
    /// keeps going until the model stops asking for tools.
    ///
    /// note: a turn, and not a call to [`Kernel::turn`]: the call that carries on after a question
    /// is answered carries on counting too. See the note on [`Kernel::turn`].
    ///
    /// note: This is a stop, not a policy: reaching it just ends the turn, and calling
    /// [`Kernel::turn`] again resumes exactly where it left off.
    ///
    /// note: The kernel starts with this value; [`Kernel::set_max_requests_per_turn`] changes it
    /// while the session runs.
    pub max_requests_per_turn: Option<usize>,
    /// Whether a request the kernel can already see is longer than the model will read is refused
    /// here instead of being sent.
    ///
    /// note: On by default: the round trip buys nothing, because the endpoint answers an over-limit
    /// request with an error after reading the whole of it. The error is
    /// [`Error::TooLong`](crate::Error::TooLong), carrying the same two numbers as a provider's
    /// refusal, and [`Event::StepFailed`] carries them too. A client can then say how much to
    /// remove without telling the two refusals apart.
    ///
    /// note: It acts on an estimate, so it is a knob rather than a rule. [`TokenCounter`] is an
    /// estimator and this crate holds no tokenizer; a counter that reads high for a model would
    /// refuse a request that endpoint would have accepted, and the fix is not to edit the context
    /// until an estimate is happy. Turning it off sends every request: the cost is one round trip
    /// per refusal, and what is kept is the endpoint's own verdict.
    ///
    /// note: Nothing is refused when the provider does not say what the model holds. The comparison
    /// needs both halves; inventing the second applies one model's limit to another.
    pub refuse_oversized_requests: bool,
    /// The default limit (in bytes) applied to tool output, used for tools whose
    /// [`ToolSpec::output_limit`] is `None`; `None` means tool output is never truncated by
    /// the kernel.
    ///
    /// note: Truncation is recorded on the context item and reported in
    /// [`Event::ToolFinished`], so it can never happen silently.
    ///
    /// note: where the kernel starts; [`Kernel::set_default_tool_output_limit`] changes it while
    /// the session runs.
    ///
    /// [`Kernel::set_default_tool_output_limit`]: crate::Kernel::set_default_tool_output_limit
    pub default_tool_output_limit: Option<usize>,
    /// Whether the whole of a truncated tool output is kept in the context, excluded, beside the
    /// shortened copy the model is shown.
    ///
    /// note: On by default: an output limit decides what the model sees, not whether to destroy
    /// what the tool said. With it on, the whole output is an
    /// [`ContextState::Excluded`](crate::ContextState::Excluded) item, listed and inspectable like
    /// any other, and restoring it is a [`Kernel::set_state`] like any other.
    ///
    /// note: Keeping it costs a pointer, not a copy: the excluded item shares the tool's
    /// allocation, and only the shortened copy is new. The real cost is retention: with this off,
    /// the whole output is dropped once shortened. Turn it off when a tool can produce more than
    /// you are willing to hold. Truncation is still reported either way; only reversibility
    /// changes.
    pub keep_truncated_output: bool,
    /// Whether the payload a [`Provider`](crate::Provider) renders for each request is broadcast
    /// and recorded, as [`Event::ModelPayload`].
    ///
    /// note: Off by default because of memory cost, which cannot be worked around. A stateless chat
    /// API re-sends the whole conversation every time, so payload n contains payload n-1; recording
    /// them all is quadratic in the number of requests, and the log is a live structure in RAM that
    /// nothing can compress.
    ///
    /// note: On disk it is cheaper, because repeated prefixes compress well with `xz`. To keep
    /// them, use [`Kernel::drain_history`] on a schedule: take the records, compress them
    /// somewhere, and stop paying for them in memory.
    ///
    /// note: With it off, [`Kernel::preview_payload`] still renders on demand; nothing is hidden,
    /// only not kept.
    pub record_payloads: bool,
    /// Whether the tools a model asks for in one turn run at the same time.
    ///
    /// note: Off by default, and not because of performance. With it off, calls run one at a time
    /// in the order the model asked for them, so two edits to the same file apply in sequence.
    /// Turning it on gives that up. The kernel cannot tell whether a model's calls are independent,
    /// and no [`Tool`](crate::Tool) declares it, so you must decide on purpose.
    ///
    /// note: The context does not change. Results are recorded in the order the model asked for
    /// them, once they have all finished, so a session looks the same in either mode. What varies
    /// is the order of [`Event::ToolStarted`] and [`Event::ToolOutput`], which interleave, and no
    /// [`Event::ToolFinished`] arrives until the slowest call is done.
    ///
    /// note: This is the only place the kernel spawns tasks, and only for the length of the step
    /// that spawned them. Dropping the future driving [`Kernel::step`] aborts them, so a cancelled
    /// turn is cancelled here too.
    ///
    /// note: What an interrupt can stop also changes. Run one at a time, a batch has a queue, and
    /// [`Kernel::interrupt`] empties it: every call that had not begun is recorded as interrupted
    /// before it was made. Run together, all calls start before the first finishes, so there is no
    /// queue. The only thing an interrupt can reach is a [`Tool`](crate::Tool) that checks
    /// [`OutputSink::is_interrupted`](crate::OutputSink::is_interrupted) itself. Keep that in mind
    /// before running a batch of `rm` calls together.
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
            refuse_oversized_requests: true,
            default_tool_output_limit: None,
            keep_truncated_output: true,
            record_payloads: false,
            parallel_tool_calls: false,
        }
    }
}
