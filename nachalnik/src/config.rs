//! The knobs, with the reasoning beside each default that needs it.

#[cfg(doc)]
use crate::{Event, Kernel, TokenCounter, ToolSpec};

/// The kernel's configuration. See the source of [`Config::default`] for the defaults.
///
/// note: no field here changes what the model is told; they decide how the loop behaves and how
/// much of it is recorded. The kernel ships no system prompt, instructions or templates; everything
/// the model reads is context somebody pushed (see [`Kernel::push`]).
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
    /// Whether [`Event::ModelDelta`] and [`Event::ToolOutput`] events, the ones
    /// [`Event::is_progress`] names, are appended to the session log.
    ///
    /// note: they are always broadcast; this decides only whether streaming fragments are also
    /// kept, which is rarely useful (the assembled result is recorded either way) and can dominate
    /// the log.
    pub record_progress: bool,
    /// How many context snapshots are retained for [`Kernel::undo`]; `0` disables undo.
    pub context_undo_depth: usize,
    /// The maximum number of requests a turn may send before it hands control back; `None` means it
    /// keeps going until the model stops asking for tools.
    ///
    /// note: per turn, not per call to [`Kernel::turn`]: the call that carries on after a question
    /// is answered keeps counting; see the note on [`Kernel::turn`]. Reaching it only ends the
    /// turn, and calling [`Kernel::turn`] again resumes where it left off.
    ///
    /// note: the kernel starts with this value; [`Kernel::set_max_requests_per_turn`] changes it
    /// during the session.
    pub max_requests_per_turn: Option<usize>,
    /// Whether a request the kernel can already see is longer than the model will read is refused
    /// here instead of being sent.
    ///
    /// note: on by default, because the round trip buys nothing: the endpoint reads the whole
    /// over-limit request and answers with an error. The error is
    /// [`Error::TooLong`](crate::Error::TooLong), with the same two numbers as a provider's
    /// refusal, also carried by [`Event::StepFailed`], so a client can say how much to remove
    /// without telling the two refusals apart.
    ///
    /// note: it acts on an estimate, so it is a setting rather than a rule. [`TokenCounter`] is an
    /// estimator and this crate holds no tokenizer, so a counter that reads high for a model
    /// refuses a request that endpoint would accept, and editing the context until the estimate is
    /// happy is not the fix. Turned off, every request goes out: the cost is one round trip per
    /// refusal, and the endpoint's own verdict is what counts.
    ///
    /// note: nothing is refused when the provider does not say what the model holds: the comparison
    /// needs both halves, and inventing the second applies one model's limit to another.
    pub refuse_oversized_requests: bool,
    /// The default limit (in bytes) applied to tool output, used for tools whose
    /// [`ToolSpec::output_limit`] is `None`; `None` means tool output is never truncated by the
    /// kernel.
    ///
    /// note: truncation is recorded on the context item and reported in [`Event::ToolFinished`], so
    /// it is never silent.
    ///
    /// note: the kernel starts here; [`Kernel::set_default_tool_output_limit`] changes it during
    /// the session.
    ///
    /// [`Kernel::set_default_tool_output_limit`]: crate::Kernel::set_default_tool_output_limit
    pub default_tool_output_limit: Option<usize>,
    /// Whether the whole of a truncated tool output is kept in the context, excluded, beside the
    /// shortened copy the model is shown.
    ///
    /// note: on by default: an output limit decides what the model sees, not whether to destroy
    /// what the tool returned. The whole output becomes an
    /// [`ContextState::Excluded`](crate::ContextState::Excluded) item, listed and inspectable, and
    /// restored with [`Kernel::set_state`] like any other.
    ///
    /// note: keeping it costs a pointer, not a copy (the excluded item shares the tool's
    /// allocation), but it costs retention. Turn it off when a tool can produce more than you are
    /// willing to hold, and the whole output is dropped once shortened. Truncation is reported
    /// either way; only reversibility changes.
    pub keep_truncated_output: bool,
    /// Whether the payload a [`Provider`](crate::Provider) renders for each request is broadcast
    /// and recorded, as [`Event::ModelPayload`].
    ///
    /// note: off by default because of memory, which cannot be worked around: a stateless chat API
    /// resends the whole conversation, so payload n contains payload n-1, recording them all is
    /// quadratic in the number of requests, and the log is a live structure in RAM.
    ///
    /// note: on disk they compress well, since repeated prefixes are what `xz` does best. To keep
    /// them, take the records with [`Kernel::drain_history`] on a schedule, compress them
    /// elsewhere, and stop holding them in memory.
    ///
    /// note: with it off, [`Kernel::preview_payload`] still renders on demand; nothing is hidden,
    /// only not kept.
    pub record_payloads: bool,
    /// Whether the tools a model asks for in one turn run at the same time.
    ///
    /// note: off by default, and not for performance: run one at a time, calls execute in the order
    /// the model asked, so two edits to one file apply in sequence. Nothing tells the kernel
    /// whether calls are independent, and no [`Tool`](crate::Tool) declares it, so turning this on
    /// is your decision.
    ///
    /// note: the context is the same either way: results are recorded in the order asked, once all
    /// have finished. Only the order of [`Event::ToolStarted`] and [`Event::ToolOutput`] varies, as
    /// they interleave, and no [`Event::ToolFinished`] arrives until the slowest call is done.
    ///
    /// note: the only place the kernel spawns tasks, for the length of the step that spawned them;
    /// dropping the future driving [`Kernel::step`] aborts them.
    ///
    /// note: it also changes what an interrupt can stop. One at a time, a batch has a queue, and
    /// [`Kernel::interrupt`] empties it, recording every call not yet begun as interrupted.
    /// Together, every call starts before the first finishes, so an interrupt reaches only a
    /// [`Tool`](crate::Tool) that checks
    /// [`OutputSink::is_interrupted`](crate::OutputSink::is_interrupted) itself; keep that in mind
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
