//! The commands about the model a session talks to and how: `/endpoint`, `/model`, `/models`,
//! `/params` and `/limit`.

use nachalnik::Calibration;

use crate::{
    app::{App, Speaker, text::thousands},
    tools::Limits,
};

/// Parameters that shape how an answer is delivered rather than what the model does, which every
/// endpoint of the dialect reads and no model lists.
///
/// note: kept out of what is said to be ignored, because they are not: `stream: false` turns the
/// answer into one body, on a model whose list has no word for it.
const TRANSPORT: [&str; 2] = ["stream", "stream_options"];

/// The fields of a request that any dialect builds from the session - the context, the tools
/// and the model - which each leaves off the wire when a parameter names one.
///
/// note: refused here as well, and in the union of them, because a dialect skipping one says
/// nothing where a person can see it: set and then quietly not sent, it would read on the
/// `/params` line as a parameter in force. `contents` means nothing to the other dialects, so
/// refusing it there costs nobody anything.
const BUILT: [&str; 7] = [
    "model",
    "messages",
    "tools",
    "contents",
    "systemInstruction",
    "system",
    "input",
];

impl App {
    /// `/endpoint`: where requests go, said with nothing given, or moved to an address and
    /// optionally a model there.
    pub(super) fn endpoint(&mut self, rest: &str) {
        if rest.is_empty() {
            let endpoint = crate::endpoint::shown(&self.provider.endpoint());
            self.say(Speaker::Note, format!("requests go to {endpoint}"));
            return;
        }
        // `URL MODEL`, because a model belongs to the address that serves it: switching
        // one and keeping the other is how a session ends up asking the ollama on this
        // machine for `gemini-3.6-flash`. Given no model the old name is kept, and the new
        // endpoint is asked whether it has one by that name
        let (url, model) = match rest.split_once(char::is_whitespace) {
            Some((url, model)) => (url.to_owned(), Some(model.trim().to_owned())),
            None => (rest.to_owned(), None),
        };
        // refused before anything is announced or changed, so the session keeps talking to
        // the address it had rather than to one no request can reach
        if !crate::endpoint::is_an_address(&url) {
            self.say(
                Speaker::Error,
                format!(
                    "`{url}` is not an address: it wants http:// or https:// and a host, \
                     and no `?` or `#`, as in `/endpoint http://localhost:11434/v1`; \
                     requests still go to {}",
                    crate::endpoint::shown(&self.provider.endpoint())
                ),
            );
            return;
        }
        let provider = self.provider.clone();
        // the address the requests will go to, which is this without the trailing `/` a
        // copied address often carries - said as typed, it named one the provider trims
        let shown = crate::endpoint::shown(url.trim_end_matches('/'));
        self.say(
            Speaker::Note,
            match (&model, self.kernel.model_info()) {
                (Some(model), _) => format!("{model} at {shown}, from now on"),
                (None, Some(info)) => format!(
                    "requests now go to {shown}, still asking for {}; the key is the one \
                     this started with",
                    info.model
                ),
                // a session that has not picked one yet: there is no name to carry over,
                // and saying it is "still asking for" nothing reads as a model called
                // nothing rather than as the gap it is
                (None, None) => format!(
                    "requests now go to {shown}; there is still no model, and `/models` \
                     lists what this one serves"
                ),
            },
        );
        // for the reason `/model` drops them, and more so: the same name at a different
        // address is a different model, and this is the command that says so
        self.forget_the_last_model();
        // the new endpoint has a context limit of its own, and a list of what it serves;
        // both are round trips, the screen should not stop for them, and the next line does
        // and then the kernel is told, as `/model` tells it
        //
        // note: which compares what the provider reports about itself, the address among
        // it - so a `/endpoint` that keeps the model's name is a `model.changed` in the
        // record like one that does not, and a session resumed from it can say where it
        // had been talking
        let kernel = self.kernel.clone();
        self.switch(async move {
            provider.set_endpoint(url, model).await;
            kernel.provider_changed();
        });
    }

    /// `/params`: one parameter set to a JSON value or taken away with `null`, and then what is
    /// set and what the model makes of it.
    pub(super) fn params(&mut self, rest: &str) {
        // a key alone is half a command, and taken as `/params` it listed the parameters
        // as if it had done something - which reads as the key having been set, or taken
        // away, depending on what was meant
        let named = rest.trim();
        if !named.is_empty() && !named.contains(char::is_whitespace) {
            self.say(
                Speaker::Error,
                format!(
                    "`/params {named}` needs a JSON value after it; `/params {named} \
                     null` takes it away, and `/params` on its own lists them"
                ),
            );
            return;
        }
        if let Some((key, value)) = rest.split_once(' ') {
            let key = key.trim();
            let value = match serde_json::from_str(value.trim()) {
                Ok(value) => value,
                Err(e) => {
                    self.say(Speaker::Error, format!("{key} needs a JSON value: {e}"));
                    return;
                }
            };
            let mut params = self.kernel.params();
            match value {
                // note: `null` takes a parameter away rather than sending one. An absent
                // field leaves the choice to the endpoint, which is what a null asks for
                // where one is taken at all, and without this nothing once set could be
                // taken back short of `/restart`. Ahead of the refusal below, so that one
                // arriving in a snapshot can be taken away too
                serde_json::Value::Null => {
                    params.remove(key);
                }
                _ if BUILT.contains(&key) => {
                    self.say(
                        Speaker::Error,
                        format!(
                            "{key} is built from the session rather than set, so a \
                             parameter of that name is not sent"
                        ),
                    );
                    return;
                }
                value => {
                    params.insert(key.to_owned(), value);
                }
            }
            self.kernel.set_params(params);
        }
        let params = self.kernel.params();
        let json = serde_json::to_string(&params).unwrap_or_default();
        self.say(Speaker::Note, format!("parameters: {json}"));

        // note: before the listing is consulted, because this is not a claim about what
        // the model takes - it is one about what this program can read back. A parameter
        // that makes the stream send the whole answer again is one whose effect lands in
        // the transcript, the context and the log, and an endpoint publishing no list at
        // all (ollama, a bare proxy) returns early below and would never have been told
        for (name, does) in nachalnik_providers::openai::NOT_A_STREAM {
            if params.get(name).is_some_and(|set| set != false) {
                self.say(Speaker::Error, format!("{name} {does}"));
            }
        }

        // an empty list means the endpoint published none, not that the model takes none;
        // ollama and a bare OpenAI-compatible proxy both say nothing here, and inventing
        // a restriction out of their silence would be worse than saying nothing back
        let Some(info) = self
            .kernel
            .model_info()
            .filter(|it| !it.parameters.is_empty())
        else {
            return;
        };
        let takes = |key: &str| info.parameters.iter().any(|name| name == key);

        // the failure worth naming: a parameter the model does not take is not refused.
        // It is sent, it is ignored, and the run it was supposed to change is the same
        // run it would have been - a `seed` that buys no reproducibility, silently
        let ignored: Vec<&str> = params
            .keys()
            .map(String::as_str)
            .filter(|key| !takes(key) && !TRANSPORT.contains(key))
            .collect();
        if !ignored.is_empty() {
            // note: two messages, because the list supports two different claims. Where it
            // is everything the model takes, a parameter missing from it is sent and
            // ignored, and that is what the error says. Where the endpoint published its
            // *sampling* parameters only, the same absence settles nothing:
            // `reasoning_effort` is not among `mercury-2.5`'s and is read anyway, so
            // reporting it as ignored would be this program inventing a restriction out of
            // a list that never claimed to be complete. It says what it actually knows,
            // and an error is downgraded to a note with it - not knowing is not a fault
            let (speaker, said) = match self.provider.lists_every_parameter() {
                true => (
                    Speaker::Error,
                    format!(
                        "{} does not list {}: sent, and ignored",
                        info.model,
                        ignored.join(", ")
                    ),
                ),
                false => (
                    Speaker::Note,
                    format!(
                        "{} publishes its sampling parameters only, so nothing here says \
                         what becomes of {}: sent, and unchecked",
                        info.model,
                        ignored.join(", ")
                    ),
                ),
            };
            self.say(speaker, said);
        }

        // a bound is worth saying of one that is set, too, since that is where it can be
        // crossed; what becomes of a value over it is the endpoint's to say, not this
        for (name, set) in &params {
            let over = self.provider.published(name).maximum.filter(|most| {
                matches!((set.as_f64(), most.as_f64()), (Some(set), Some(most)) if set > most)
            });
            if let Some(most) = over {
                self.say(
                    Speaker::Note,
                    format!(
                        "{name} is {set}, and {} publishes at most {most}",
                        info.model
                    ),
                );
            }
        }

        let spare: Vec<&str> = info
            .parameters
            .iter()
            .map(String::as_str)
            // `tools` is on some endpoints' lists, and offering one this command refuses
            // would be a line contradicting the next
            .filter(|name| !params.contains_key(*name) && !BUILT.contains(name))
            .collect();
        if !spare.is_empty() {
            let all = match self.provider.lists_every_parameter() {
                true => "also takes",
                false => "also takes, of the ones it publishes",
            };
            // note: one to a line, with what the endpoint published about each beside it.
            // A type or a range it did not publish is not made up here: a default is a
            // value, so its type shows in it, and that is all there is to go on
            let wide = spare.iter().map(|name| name.len()).max().unwrap_or(0) + 2;
            let rows = spare
                .iter()
                .map(|name| {
                    let published = self.provider.published(name);
                    let facts = [
                        published.default.map(|it| format!("default {it}")),
                        published.maximum.map(|it| format!("at most {it}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                    format!("  {name:<wide$}{facts}").trim_end().to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.say(Speaker::Note, format!("{} {all}:\n{rows}", info.model));
        }
    }

    /// Drops the two figures that were one model's tokenizer counting one model's request.
    ///
    /// note: the anchor is what the provider charged for the last request, and carrying it across
    /// is the corner reporting what the *last* model would have charged for a request going to a
    /// different one. `App::anchored` says it falls back after a change of model; this is the line
    /// that makes that true.
    ///
    /// note: and the correction, for the same reason. `Calibrating` is the ratio between what this
    /// counter guessed and what a provider billed, cumulative over every observation - so a scale
    /// learnt from one tokenizer goes on correcting the next one's figures, and a fresh
    /// observation from the new model is averaged into the old model's totals rather than
    /// replacing them, settling on a scale that is neither model's. It is reset through
    /// `Kernel::recalibrate` because that recounts the items as well, which is the half that keeps
    /// the `sending` column and the budget on one scale.
    ///
    /// note: and what has been said once about the endpoint - that it hides its reasoning, that it
    /// reports no usage - because it was said about the endpoint that is gone, and the next one may
    /// be just the same.
    pub(super) fn forget_the_last_model(&mut self) {
        self.anchor = None;
        self.kernel.recalibrate(Calibration::default());
        (self.thought_unseen, self.unreported) = (false, false);
    }

    /// Shows the output limits, or changes one.
    ///
    /// note: a limit can be right for the other things a tool does and wrong for one call - a
    /// copy of the session asked three questions answers at the end of its deliberation, which is
    /// the part a limit cuts off. Without this, a person watching a result come back shortened
    /// can live with it or lose the session.
    ///
    /// note: it changes the next call, not the one already shortened, and the message says which.
    /// Nothing is lost either way: the whole of a shortened result is excluded beside the copy the
    /// model was shown, and one keystroke on the context tab sends it instead.
    ///
    /// note: a row is a **subject** - `fs:read`, `context:look` - which is the same string the
    /// permissions tab is keyed on, so a person who has read one table can read the other and
    /// `--allow fs:grep` and `/limit fs:grep` name the same thing. A tool id is not enough once
    /// one tool does five things of five different sizes.
    ///
    /// note: the table is the `Limits` map rather than the registry, and it holds a row for every
    /// subject this program's tools declare, whether or not this session offers them. That is
    /// right, because a limit set before a tool arrives is in force when it does. A row nobody has
    /// is marked, for the reason
    /// `introspect::if_offered` exists on the other side of the screen: a name in an answer reads
    /// as a thing that is there.
    ///
    /// note: what "offered" means is what the registered tools *declare*, rather than a tool id
    /// read off the front of a row. Those are not the same question, and the difference shows:
    /// `shell` declares `exec:run`, so splitting that row on its colon and looking for a tool
    /// called `exec` would mark the one row that is certainly offered.
    pub(super) fn limit(&mut self, rest: &str) {
        let offered: Vec<String> = self
            .kernel
            .tool_specs()
            .iter()
            .flat_map(|spec| spec.capabilities.iter().map(|it| it.to_string()))
            .collect();
        let table = |limits: &Limits| {
            let rows = limits.all();
            // as wide as the widest number, so eight tools read `[1]` and a dozen do not jog the
            // column the figures are in
            let wide = rows.len().to_string().len() + 2;
            rows.into_iter()
                .enumerate()
                .map(|(nth, (tool, bytes))| {
                    format!(
                        "{:<wide$} {tool:<18}{:>9} bytes{}",
                        format!("[{}]", nth + 1),
                        thousands(bytes),
                        match offered.contains(&tool) {
                            true => "",
                            false => "   · not offered",
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut words = rest.split_whitespace();
        let (Some(named), Some(bytes)) = (words.next(), words.next()) else {
            if !rest.trim().is_empty() {
                self.say(
                    Speaker::Error,
                    // `<subject>`, which is what the rows are and what the table two lines down
                    // calls them. A limit is not a tool's, because one tool does five things
                    "`/limit <subject> <bytes>`, or `/limit` on its own to see them",
                );
                return;
            }
            let body = format!(
                "{}\n\nhow much of a call's output the model is shown, keyed by the same subject \
                 its permission is. `/limit <subject> <bytes>` changes one, by name or by the \
                 number beside it, from the next call onwards; the whole of anything already \
                 shortened is excluded beside it, {} from being sent instead.{}",
                table(&self.limits),
                // `/restore` is the same act as the key: both make the excluded copy active
                match self.keys {
                    true => "one `space` on the context tab away",
                    false => "one `/restore` with its number away",
                },
                match self
                    .limits
                    .all()
                    .iter()
                    .any(|(subject, _)| !offered.contains(subject))
                {
                    // said once, under the table, rather than argued on every marked row - and
                    // true whichever rows are marked, since which tools a session is missing is
                    // not something this sentence gets to assume
                    true =>
                        " A row marked `not offered` is a limit held for something no tool here \
                         declares; it is in force the moment one does, and `/tools toggle` is \
                         what offers back a tool that was turned off.",
                    false => "",
                }
            );
            self.preview("the output limits", body);
            return;
        };

        // the number the listing prints, or the name the model calls. A row out of range falls
        // through as a name and is answered with the listing, which is where the range is
        let rows = self.limits.all();
        let tool = match named.parse::<usize>() {
            Ok(nth) if (1..=rows.len()).contains(&nth) => rows[nth - 1].0.clone(),
            _ => named.to_owned(),
        };
        let tool = tool.as_str();

        let Ok(bytes) = bytes.parse::<usize>() else {
            self.say(
                Speaker::Error,
                format!("`{bytes}` is not a number of bytes"),
            );
            return;
        };
        // a limit of nothing is a tool whose every answer is a marker, which is not a limit
        // anybody means; dropping the tool is what "say nothing" is spelled
        if bytes == 0 {
            self.say(
                Speaker::Error,
                "0 would send the model nothing but a truncation marker; `/tools toggle ID` is \
                 how a tool stops being offered",
            );
            return;
        }

        match self.limits.set(tool, bytes) {
            Some(was) => self.say(
                Speaker::Note,
                format!(
                    "`{tool}` was cut at {} bytes and is now cut at {}, from its next call \
                     onwards{}",
                    thousands(was),
                    thousands(bytes),
                    // a limit that took, on something nothing here declares - which is a real
                    // thing to set up ahead of a toggle and a confusing thing to be told nothing
                    // about, since "from its next call onwards" implies there will be one
                    match offered.iter().any(|it| it == tool) {
                        true => String::new(),
                        false => format!(
                            ". nothing in this session declares `{tool}`, so it has no next call \
                             until something does"
                        ),
                    }
                ),
            ),
            None => self.say(
                Speaker::Error,
                format!("nothing here limits `{tool}`; `/limit` on its own lists what is limited"),
            ),
        }
    }

    /// Shows what `/models` came back with: on a page at the desk, and said to anybody else.
    ///
    /// note: said rather than opened where whoever asked has no keys of this program's - down a
    /// pipe, or from a client. A page used to reach them as the answer to their line, and the list
    /// now comes back after the answer has gone; the program's voice is what reaches them later.
    pub(super) fn show_models(&mut self, listed: Vec<String>, filter: &str, keys: bool) {
        // note: "did not answer with" rather than "lists no": an empty answer is an address that
        // publishes no listing *or* one that could not be reached, and the provider does not say
        // which
        if listed.is_empty() {
            self.say(
                Speaker::Error,
                format!(
                    "{} did not answer with a list of models: it may publish none, or not be \
                     reachable",
                    self.provider.host()
                ),
            );
            return;
        }

        let shown: Vec<&String> = listed
            .iter()
            .filter(|name| filter.is_empty() || name.to_lowercase().contains(filter))
            .collect();
        if shown.is_empty() {
            self.say(
                Speaker::Note,
                format!("none of the {} listed match `{filter}`", listed.len()),
            );
            return;
        }

        // the one in use is marked where it stands rather than pulled to the top, so the list
        // keeps the order the endpoint gave it
        let current = self.kernel.model_info().map(|info| info.model);
        let body = shown
            .iter()
            .map(|name| {
                let mark = match &current {
                    Some(model) if nachalnik_providers::same_model(name, model) => "▸",
                    _ => " ",
                };
                format!("{mark} {name}")
            })
            .collect::<Vec<_>>()
            .join("\n");

        // no address in the title: it is one `/endpoint` away, and the room it costs is the room
        // the line below needs to say what to do with any of this
        let title = match filter.is_empty() {
            true => format!(" {} models", shown.len()),
            false => format!(" {} of {} matching `{filter}`", shown.len(), listed.len()),
        };
        match keys {
            true => self.preview(format!("{title} · /model ID switches "), body),
            false => self.say(
                Speaker::Note,
                format!("---{title} · /model ID switches ---\n{body}"),
            ),
        }
    }
}
