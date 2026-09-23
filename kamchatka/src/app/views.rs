//! What the screen asks of a session: the question waiting, what the next request does with each
//! item, the rows the context and permissions tabs draw, and what a key is about to change.
//!
//! note: an `impl App` of its own, the way `keys.rs` and `transcript.rs` are. These are the
//! answers a view reads and nothing else does: `ui/` decides nothing, so every question it has
//! to ask is one of these, and holding them together is what makes that claim checkable. They
//! read the kernel afresh rather than a copy kept here - a pane that drew a remembered figure
//! would be a second account of the request, and the two would disagree the moment anything
//! moved.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use nachalnik::{
    Budget, Capability, Content, ContextId, ContextItem, Domain, Grant, PermissionRequest, Verdict,
};

use super::{App, Going, Tab, Traced, text::thousands, when};
use crate::tools::Subject;

/// One row of the permissions tab: a capability, a path rule or a rule about one tool action, what
/// the policy will answer about it, and the tools that would be affected.
pub struct Stance {
    /// What the row is about: a capability, or a pattern the paths are matched against.
    pub subject: Subject,
    /// What the policy answers about it today.
    pub verdict: Verdict,
    /// The registered tools that declare it, in the order the model is offered them.
    pub tools: Vec<String>,
    /// The registered tools the policy judges against it only sometimes, by looking at the call.
    ///
    /// note: `network` and `shell` are the pair this exists for. No tool here declares `network` -
    /// a model that wants the network writes `curl` - so without this the row would read `nothing
    /// registered needs it` beside a verdict of `deny`, a restriction that is not there. What is
    /// there is [`crate::tools::Careful`] reading the command, and that is what this says.
    pub sometimes: Vec<String>,
    /// What makes it *that* time, in a clause the pane puts after [`Stance::sometimes`].
    ///
    /// note: a field rather than a sentence fixed in the pane, so that a subject judged only
    /// sometimes can say what makes it that time. The one case today is `shell`, judged against
    /// `net:reach` when the command reaches for it.
    pub when: &'static str,
}

impl Stance {
    /// Whether somebody has actually answered about this, as opposed to it being the default.
    pub fn is_decided(&self) -> bool {
        self.verdict != Verdict::Ask
    }
}

impl App {
    /// What the next request does with each item, for the screen that is about to draw it.
    ///
    /// note: one line, because the answer is the kernel's rather than the session's - see
    /// [`Going::of`], which is where it lives and where `context` reads it from too. The model's
    /// account of its own budget and the person's are one piece of arithmetic; two would disagree
    /// about every turn holding thinking the endpoint will not take.
    pub fn going(&self) -> Going {
        Going::of(&self.kernel)
    }

    /// What the next request is expected to cost, taken from what the last one really cost.
    ///
    /// note: **the anchored figure**, and the arithmetic is one line of intent: the provider's
    /// number, plus what the context estimates now, minus what the estimator says the items
    /// that number covered would cost now. An item that has not moved appears in both estimates
    /// and cancels, so it contributes its *measured* cost and no error at all; only what
    /// changed since the last request is estimated. Add a message to a hundred-thousand-token
    /// context and the error is a few tokens rather than a few hundred.
    ///
    /// note: both estimates are taken *now*, with the counter as it currently stands, which is
    /// what makes the cancellation exact. Storing what each item was estimated at when the
    /// request went out would not: `Calibrating` revises its scale on the way past, when the
    /// very response this figure comes from is observed, so every stored figure would be in
    /// older money than the ones it is subtracted from and a context that had not changed at
    /// all would drift.
    ///
    /// note: `None` before any response, on an endpoint that reports no usage, and after a
    /// change of model until the next response - all three being cases where there is nothing
    /// exact to build on, and the caller falls back to [`Budget::used`].
    ///
    /// note: what it does not catch, until the next request re-anchors it: a tool added or
    /// dropped, since the schemas are inside the provider's figure and are not itemised in it.
    /// The context is the part that moves.
    pub fn anchored(&self, going: &Going, budget: &Budget) -> Option<usize> {
        let anchor = self.anchor.as_ref()?;
        // the markers are what they were - see `Anchor::markers` - and everything whose content
        // was really in that request is re-estimated now, which is what makes an item that has
        // not moved cancel against itself exactly
        let covered: usize = anchor.markers
            + anchor
                .sent
                .iter()
                .map(|id| self.valued(going, *id))
                .sum::<usize>();

        Some(
            (anchor.reported as i64 + budget.context_tokens as i64 - covered as i64).max(0)
                as usize,
        )
    }

    /// What one item would cost the request if it were sending its content, as the counter sees
    /// it now.
    ///
    /// Only ever asked about an item whose content really was in the anchored request - see
    /// [`Anchor::sent`] - which is what makes the middle arm below true rather than a guess.
    ///
    /// note: three answers rather than one, and the middle one is the reason. An item that is
    /// still sending its content is worth what the projection says it costs - the message it
    /// becomes, which is the figure the budget is built from. One that is *not* any more -
    /// excluded, archived, or elided since - is worth what it holds, because that is what it
    /// contributed to the provider's figure and that is what has to come back out of it; the
    /// marker standing in its place now is already counted on the other side. One that no longer
    /// exists at all, because an undo took it, is worth nothing anybody can recover, and the next
    /// request puts the accounting straight.
    fn valued(&self, going: &Going, id: ContextId) -> usize {
        match self.kernel.item(id) {
            Some(item) if going.sends_content(&item) => {
                going.costs.get(&id).copied().unwrap_or(item.tokens)
            }
            Some(item) => item.tokens,
            None => 0,
        }
    }

    /// What the message being typed would add to the next request.
    ///
    /// note: the counter rather than a rule of thumb, so that a draft is measured the same way
    /// as everything already in the context - including whatever `Calibrating` has learnt about
    /// this model. It is worth showing at all only because the figure it is added to is
    /// anchored: a draft moving a number that is itself a thousand tokens uncertain would be
    /// precision theatre.
    pub fn drafted(&self) -> usize {
        let draft = self.draft();
        match draft.trim().is_empty() || draft.starts_with('/') {
            true => 0,
            false => self.kernel.counter().count(&Content::text(draft)),
        }
    }

    /// What every item is holding out of the next request, and how many of them there are.
    ///
    /// note: [`Context::tokens_withheld`](nachalnik::Context::tokens_withheld) answers this from
    /// the item states, which is the right answer to a question about states and the wrong one
    /// here: it counts an excluded, archived or elided item and misses one the projector repaired
    /// away, because that one's state says it is sending. `/budget` and the context tab have to
    /// agree about this figure or they are two accounts of one request.
    ///
    /// note: and [`Going::held_back`] per item rather than the whole of one that is not going,
    /// because "is this item going" is not a yes or a no. A turn whose thinking the endpoint will
    /// not take back is going and is holding tens of thousands of tokens, and a count of the items
    /// not going would find none of it.
    pub(crate) fn withheld(&self, going: &Going) -> (usize, usize) {
        self.kernel
            .items()
            .iter()
            .map(|item| going.held_back(item))
            .filter(|held| *held != 0)
            .fold((0, 0), |(tokens, count), held| (tokens + held, count + 1))
    }

    /// Answers one waiting question, and does the rest of what answering it entails.
    ///
    /// note: here rather than beside the keys, because there are two drivers and only one of them
    /// has any. With everything answering means beyond `Kernel::decide` in the key handler, a
    /// headless run answering `allow` to a `curl` would decide it and grant nothing, and the
    /// command would run with the network cut - the failure the grant below exists to prevent. A
    /// driver should not be able to answer a question halfway by forgetting a step it never knew
    /// about.
    ///
    /// note: the half of an answer that is about the *call*. What is about the session - the
    /// `always` sweep, the questions queued behind this one, the turn nobody is driving, and the
    /// `acted` flag the trace reads - is [`App::decide`], one level up, which every loop answers
    /// through. This stays separate because a caller with a `PermissionRequest` already in
    /// hand should not have to find its identifier again to use it.
    pub fn answer(&mut self, request: &PermissionRequest, grant: Grant) -> Result<(), String> {
        // saying yes to a command that reaches for the network is permission for *that* command,
        // and the sandbox has to hear about it. Read through the wrapper, because `shell` takes
        // its arguments inside a `call` object and there is no `cmd` on the outside of one
        if grant == Grant::Allow
            && request.capabilities.contains(&Capability::exec("run"))
            && crate::tools::ops::inner(&request.args)
                .unwrap_or(std::borrow::Cow::Borrowed(&request.args))
                .get("cmd")
                .and_then(|cmd| cmd.as_str())
                .is_some_and(crate::tools::reaches_the_network)
        {
            self.policy.grant_the_network(&request.call);
        }

        // the state the kernel lands in is the caller's to read off the events like any other;
        // neither driver wants it back from here
        self.kernel
            .decide(request.id, grant)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Where the advisor put the command a question is about, if it was asked and answered.
    ///
    /// note: read at draw time out of what the advisor wrote down while the verdict was being
    /// worked out, rather than asked for here. That ordering is what makes the rating simple: the
    /// kernel awaits the policy before it raises a question, so by the time there is a panel to
    /// draw the answer is either already there or is never coming, and nothing on the screen has
    /// to know about a request in flight.
    #[cfg(feature = "shell-advisor")]
    pub fn rating(&self, request: &PermissionRequest) -> Option<crate::tools::Rated> {
        self.advisor.as_ref()?.rating(&request.call)
    }

    /// Why a question is about everything a tool does, where that is because the call did not
    /// say which operation it wanted.
    ///
    /// note: the question is the only place this can be said in time to matter. By the time the
    /// tool refuses the call by name the answer has already been given, and a run with nobody at
    /// the prompt never gets that far: it reads `deny`, and the standing rule somebody wrote is
    /// one the call could not match. See `tools::ops::unnamed_operation`.
    pub fn widened(&self, request: &PermissionRequest) -> Option<String> {
        let tool = self.kernel.tool(&request.tool)?;

        crate::tools::ops::unnamed_operation(&tool.spec(), request)
    }

    /// The context items a pending call names, described the way a row on the context tab is.
    ///
    /// note: `ids: [22]` is a true account of the arguments and a useless one to be asked about.
    /// The question covers a tool that rewrites and hides pieces of the context, the overlay is
    /// covering the list those numbers refer to, and the answer is `y` or `n` - so somebody being
    /// asked whether item 22 may be elided has to already know what item 22 is. Naming them turns
    /// the question into one that can be answered on what is on the screen.
    ///
    /// note: only for the tools this program installs itself, and only because it knows what
    /// their arguments mean. `ids` on somebody else's tool is somebody else's vocabulary, and
    /// guessing at it would put a confident description of the wrong thing in front of a decision.
    /// Nothing here reaches the policy: it is the same arguments, read out.
    pub fn about(&self, request: &PermissionRequest) -> Vec<String> {
        if !matches!(request.tool.as_str(), "context" | "log") {
            return Vec::new();
        }

        let items = self.kernel.items();
        // the arguments wherever the model put them: these tools take theirs inside a wrapper, and
        // a question naming no items is a question nobody can answer on what is on the screen
        let Ok(args) = crate::tools::ops::inner(&request.args) else {
            return Vec::new();
        };
        // the same reading the tool will do, so that a call it is going to refuse is not described
        // here as one about to move things. A selector is the argument most worth expanding -
        // nobody can count `all:tool_results` off the screen this overlay is covering - and a
        // call naming its items both ways is one the tool refuses, so there is nothing to name
        let Ok(named) = crate::introspect::named(&items, &args) else {
            return Vec::new();
        };
        let named = named.ids;
        if named.is_empty() {
            return Vec::new();
        }

        let going = self.going();
        named
            .iter()
            .take(8)
            .map(|id| match items.iter().find(|item| item.id == *id) {
                None => format!("[{id}] there is no such item"),
                Some(item) => format!(
                    "[{id}] {} · {} · {} · {} tokens{}",
                    item.label,
                    item.kind.name(),
                    item.state,
                    thousands(item.tokens),
                    match going.left_out.contains_key(id) {
                        true => " · not in the next request",
                        false => "",
                    }
                ),
            })
            .chain((named.len() > 8).then(|| format!("… and {} more", named.len() - 8)))
            .collect()
    }

    /// The context items the tab is showing: all of them, or only the ones carrying content into
    /// the next request.
    ///
    /// note: the predicate is [`Going::sends_content`], which is exactly the set with a figure in
    /// the `held` column - so the toggle has one rule a person can hold in their head: it hides
    /// every row that is holding something back. That does leave out elided items, which do go
    /// into the request as a marker and do cost the marker's few tokens; showing them would be
    /// defensible on "what am I sending", but the reason somebody reaches for this is that half
    /// the list is wreckage after a compaction, and an elided row is wreckage.
    ///
    /// note: `Going`'s rather than the state's own, because a row the projector repaired away is
    /// holding everything it holds and would have survived this filter as though it were going -
    /// which is the one row somebody with the toggle on would most want to see the truth about.
    pub fn listed(&self) -> Vec<Arc<ContextItem>> {
        let items = self.kernel.items();
        let items = match self.sending_only {
            false => items,
            true => {
                let going = self.going();
                items
                    .into_iter()
                    .filter(|item| going.sends_content(item))
                    .collect()
            }
        };

        // note: the search goes here, beside `f`, and for the reason `f` is here: this is what the
        // keys count rows in. A pane filtered on the way to the screen while `context_key` still
        // indexed the whole context would select the row above the one under the cursor.
        let Some(search) = self.search.as_ref().filter(|_| self.tab == Tab::Context) else {
            return items;
        };

        items
            .into_iter()
            .filter(|item| search.matches(&Self::item_text(item)))
            .collect()
    }

    /// What a search over the context matches an item on.
    ///
    /// note: the whole of what the item holds, not the one line the row has room for. Somebody
    /// looking for the turn that mentioned a filename is looking for a word that is almost never
    /// on the first line, and a search that only saw the preview would answer that it is not
    /// there. The row still shows its preview; the match is allowed to be about more than the row
    /// can show.
    ///
    /// note: and the kind, which is a column on the screen; without it, `/tool_result` would match
    /// only the word appearing in somebody's *content*. It is the column most worth filtering on,
    /// because it is the one question a pane of eighty rows is usually being asked: which of these
    /// are the tool results, which are what the model said. `ContextKind::name` rather than a
    /// second vocabulary, so what is typed is what the column shows - and it is matched whether or
    /// not that column is drawn, since it is dropped below 84 columns and a filter that found less
    /// on a narrow terminal would be the worse surprise.
    ///
    /// note: the state is deliberately not in here. It is on the row as a mark rather than a word,
    /// so there is nothing somebody would be typing to match it, and `/exclude state:excluded` is
    /// the language for asking that question.
    fn item_text(item: &ContextItem) -> String {
        format!(
            "{} {} {}",
            item.label,
            item.kind.name(),
            item.content.to_text()
        )
    }

    /// The trace as the pane should show it: everything, or what the search left.
    ///
    /// note: a continuation - an event with no name, which is more of what the line above had to
    /// say - is kept or dropped with the event it belongs to rather than matched on its own. The
    /// alternative is a pane showing the second half of a message whose first half was filtered
    /// out from over it.
    pub fn traced(&self) -> Vec<&Traced> {
        let Some(search) = self.search.as_ref().filter(|_| self.tab == Tab::Trace) else {
            return self.trace.iter().collect();
        };

        let mut kept = Vec::new();
        let mut keeping = false;
        for event in &self.trace {
            if !event.name.is_empty() {
                keeping = search.matches(&Self::event_text(event));
            }
            if keeping {
                kept.push(event);
            }
        }

        kept
    }

    /// What a search over the trace matches an event on.
    ///
    /// note: the clock is in it, which is why the stamp is built down here rather than in the
    /// pane. "the hour it broke" is the question somebody brings to a long run, and
    /// `14:` or a date answers it only if the date and the time are among the things being
    /// matched.
    fn event_text(event: &Traced) -> String {
        match when::read_off(event.wall) {
            Some(read) => format!(
                "{} {} {} {}",
                read.date, read.time, event.name, event.detail
            ),
            None => format!("{} {}", event.name, event.detail),
        }
    }

    /// Every capability that matters here, and what would happen if a tool asked for it.
    ///
    /// note: the union of two lists, because either on its own is misleading. What the policy has
    /// been told about is not the whole story - a tool can need something nobody has mentioned,
    /// and that is exactly the row worth seeing, since it is the one that will stop and ask. And
    /// what the tools declare is not the whole story either: `network` is refused here and no
    /// built-in tool wants it, but a refusal you cannot see is not a policy you can trust.
    pub fn permissions(&self) -> Vec<Stance> {
        self.all_stances()
            .into_iter()
            .filter(Stance::is_decided)
            .collect()
    }

    /// How many subjects the policy will simply ask about, because nobody has told it otherwise.
    ///
    /// note: the tab does not list them - a row for a `.aws` rule nobody has thought about is not
    /// information - but it does say how many there are, because a screen showing two decisions
    /// and silently standing for sixteen answers would be a different kind of dishonest.
    pub fn undecided(&self) -> usize {
        self.all_stances()
            .iter()
            .filter(|row| !row.is_decided())
            .count()
    }

    /// Every subject this policy holds an opinion about, decided or not.
    ///
    /// note: the subjects first and what each one covers second, rather than filling the lists
    /// while walking the tools. Walking fills a row only where a tool declares that exact
    /// capability, so a rule about a whole domain - which is what `--allow fs` writes - would get a
    /// row saying "nothing registered needs it" beside the `fs:*` rows it answers for. A domain
    /// rule covers every tool with a capability in it; a server rule covers every tool
    /// that came from it; a path rule covers every tool that is handed a path. Asking one question
    /// per subject is how all four get answered instead of one.
    fn all_stances(&self) -> Vec<Stance> {
        let specs = self.kernel.tool_specs();
        let reaching = Subject::Capability(Capability::net("reach"));

        // every subject worth a row: a rule somebody has written, whether or not a registered tool
        // declares it; every capability the registered tools do declare and nothing has answered
        // for already; and every server their tools came from
        let ruled: BTreeMap<Subject, Verdict> = self.policy.stances().into_iter().collect();
        let mut subjects: BTreeSet<Subject> = ruled.keys().cloned().collect();

        // note: an operation whose answer comes from the domain above it is the same decision a
        // second time. `--allow log` is one rule, and a row each for `log` and `log:read`, each
        // naming the other in the column beside it, would say it twice. The domain row says it
        // and names every operation it answers for, which is what a rule is read for. One
        // somebody has answered about separately keeps its row, since that is a decision of its
        // own, and so does one nobody has decided: that is what the count of the rest is made of
        let answered_above = |capability: &Capability| {
            !ruled.contains_key(&Subject::Capability(capability.clone()))
                && ruled
                    .get(&Subject::Domain(capability.domain.clone()))
                    .is_some_and(|verdict| *verdict != Verdict::Ask)
        };
        // note: and a capability nothing here is judged by is not a question either. `mcp:call` is
        // declared by every tool from a server and `judges` puts the server's own name in its
        // place where this program spawned it, so counting it would put a subject a session run
        // `--mcp big=...` will never be asked about among the ones it will. A tool declaring it
        // with nobody holding the far end is a different matter and still counts.
        let judged = |tool: &str, capability: &Capability| {
            (self.policy).decides(&Subject::Capability(capability.clone()), tool)
        };
        for spec in &specs {
            subjects.extend(
                (spec.capabilities.iter())
                    .filter(|it| judged(&spec.id, it) && !answered_above(it))
                    .cloned()
                    .map(Subject::Capability),
            );
            if let Some(server) = self.policy.server_of(&spec.id) {
                subjects.insert(Subject::Server(server));
            }
            // a shell is judged against `net:reach` too, when the command it was handed reaches
            // for it; the policy is the one that knows, and this is the row that has to say so
            if spec.capabilities.contains(&Capability::exec("run")) {
                subjects.insert(reaching.clone());
            }
        }

        // note: a path rule binds every tool that is handed a path - the three that open one, and
        // the two that walk a directory of them - and no others: a `shell` command names its files
        // inside a string this program does not parse, and pretending otherwise would be exactly
        // the sort of check that implies more than it delivers. What `grep` and `glob` do about a
        // rule is not to open what it names; see `tools::search`
        let covers = |subject: &Subject| -> Vec<String> {
            // note: a domain rule answers for a *set of capabilities*, and those are what it is
            // worth naming. Saying which tools it reaches is true and says nothing - `--allow log`
            // would read `log  allow  log`, three times the same word - where the capabilities
            // are the thing somebody wrote the rule to decide and the thing they would look for
            // to check they got it right.
            if let Subject::Domain(domain) = subject {
                let mut inside: Vec<String> = specs
                    .iter()
                    .flat_map(|spec| spec.capabilities.iter().map(move |it| (spec, it)))
                    .filter(|(spec, it)| {
                        it.domain == *domain
                            && self
                                .policy
                                .decides(&Subject::Capability((*it).clone()), &spec.id)
                    })
                    .map(|(_, it)| it.to_string())
                    .collect();
                inside.sort_unstable();
                inside.dedup();

                return inside;
            }

            // note: and a rule about one operation answers for that operation and nothing wider.
            // Naming the tools, with one tool to a domain, reads back the subject's own first
            // half: `fs:glob  allow  fs` puts the narrow rule over the whole tool, the one
            // direction it cannot go. What a rule reaches is never broader than the rule.
            //
            // note: an empty list is how the row says nothing here is judged by it, and that is
            // the answer where every tool declaring the capability came from a server - `judges`
            // swaps `mcp:call` for the server's name, so a rule about `mcp` decides for none of
            // them however many declare it.
            if let Subject::Capability(capability) = subject {
                let anything = specs.iter().any(|spec| {
                    spec.capabilities.contains(capability) && judged(&spec.id, capability)
                });

                return match anything {
                    true => vec![capability.to_string()],
                    false => Vec::new(),
                };
            }

            specs
                .iter()
                .filter(|spec| match subject {
                    Subject::Server(name) => {
                        self.policy.server_of(&spec.id).as_deref() == Some(name.as_str())
                    }
                    Subject::Path(_) => spec.capabilities.iter().any(|it| it.domain == Domain::Fs),
                    // both handled above, because what either covers is not a list of tools
                    Subject::Capability(_) | Subject::Domain(_) => false,
                })
                .map(|spec| spec.id.clone())
                .collect()
        };

        // the ones a shell is judged against only sometimes, which is a different sentence from
        // the ones it always needs and is why they are a column of their own
        let sometimes = |subject: &Subject| -> Vec<String> {
            match *subject == reaching {
                true => specs
                    .iter()
                    .filter(|spec| spec.capabilities.contains(&Capability::exec("run")))
                    .map(|spec| spec.id.clone())
                    .collect(),
                false => Vec::new(),
            }
        };

        subjects
            .into_iter()
            .chain(
                self.policy
                    .paths()
                    .into_iter()
                    .map(|(pattern, _)| Subject::Path(pattern)),
            )
            .map(|subject| Stance {
                verdict: self.policy.stance(&subject),
                tools: covers(&subject),
                sometimes: sometimes(&subject),
                when: "when the command reaches for it",
                subject,
            })
            .collect()
    }

    /// What the shell can reach, in one line, or `None` if nothing here runs commands.
    pub fn confinement(&self) -> Option<String> {
        if !self.shell_is_live() {
            return None;
        }

        Some(match self.confinement.is_confined() {
            true => format!("shell: {}", self.confinement),
            false => "shell: a command can do any of these".to_owned(),
        })
    }

    /// What the policy in force is called, short enough to put at the top of a screen.
    ///
    /// note: asked of the kernel rather than of [`App::policy`], because what the permissions tab
    /// is reporting is the policy the *runtime* will consult - the same answer `/seams` gives, and
    /// the one that would notice if the two ever came apart.
    ///
    /// note: the last segment of the path. `PermissionPolicy::name` defaults to the implementing
    /// type's own path, which is right for `/seams` - a panel whose whole subject is which types
    /// are plugged in - and spends thirty columns of a list saying `kamchatka::tools::Careful`
    /// where `Careful` is the part anybody reads.
    pub fn policy_name(&self) -> String {
        let name = self.kernel.policy().name();

        name.rsplit("::").next().unwrap_or(name).to_owned()
    }

    /// Whether a registered tool can run commands, and the policy has not refused it outright.
    ///
    /// note: the question the permissions tab has to answer honestly. `Capability::exec("run")`
    /// subsumes every other capability - a command reads, writes and reaches the network - so
    /// while one is on the list and not denied, every other row is what a *tool* declares rather
    /// than what can happen, unless something is actually confining it.
    pub fn shell_is_live(&self) -> bool {
        self.policy
            .stance(&Subject::Capability(Capability::exec("run")))
            != Verdict::Deny
            && self
                .kernel
                .tool_specs()
                .iter()
                .any(|spec| spec.capabilities.contains(&Capability::exec("run")))
    }
}
