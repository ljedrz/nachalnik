//! The commands about what the model may do and what it costs: `/tools`, `/permissions` and
//! `/spend`.

use crate::app::{
    App, Speaker, Tab,
    text::{NOTHING_DECIDED, plural, thousands, verdict_word},
};

impl App {
    /// Stops offering one of the tools, or offers it again.
    ///
    /// note: one word covers both directions and every tool there is, including the ones an MCP
    /// server brought - see [`App::toggle`]. A command that could only drop a tool would have no
    /// way back: a session that dropped `fs` would have dropped it.
    ///
    /// note: it names what it did in terms of the *next request*, because that is when it takes
    /// effect and because it is the thing a person is usually doing this for. Nothing about the
    /// session as it stands changes.
    pub(super) fn toggle_tool(&mut self, id: &str) {
        if id.is_empty() {
            self.say(
                Speaker::Error,
                "`/tools toggle ID` stops offering a tool, or offers it again; `/tools` lists \
                 them and marks the ones that are off",
            );
            return;
        }

        match self.toggle(id) {
            Some(true) => self.say(
                Speaker::Note,
                format!("`{id}` is offered again, from the next request"),
            ),
            Some(false) => self.say(
                Speaker::Note,
                format!("`{id}` is no longer offered; the next request will not mention it"),
            ),
            None => self.say(Speaker::Error, format!("there is no tool called `{id}`")),
        }
    }

    /// What the model is offered, and what this session is holding back.
    ///
    /// note: both, since `toggle` goes both ways: a list of what is on tells somebody how to turn
    /// a tool off and nothing about how to get it back, and the name of a tool that is off is
    /// precisely what they would have to remember. They are one list with a mark rather than two,
    /// because what is being read is one question - what can this model do - and a second list
    /// under a heading is a second place to look for a name.
    pub(super) fn tools(&mut self) {
        let offered = self
            .kernel
            .tool_specs()
            .into_iter()
            .map(|spec| (true, spec));
        let shelved = self
            .shelved
            .values()
            .map(|tool| (false, tool.spec()))
            .collect::<Vec<_>>();
        let mut all: Vec<_> = offered.chain(shelved).collect();
        all.sort_by(|(_, one), (_, two)| one.id.cmp(&two.id));

        let body = all
            .into_iter()
            .map(|(on, spec)| {
                let capabilities: Vec<_> =
                    spec.capabilities.iter().map(|c| c.to_string()).collect();
                let mark = match on {
                    true => "▸",
                    false => "·",
                };
                format!(
                    "{mark} {:<22}{}\n{:<24}[{}]\n",
                    spec.id,
                    spec.description,
                    "",
                    capabilities.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.preview(
            "what the model is offered · /tools toggle ID turns one off",
            body,
        );
    }

    /// What the policy will answer about, and where the tab is to change any of it.
    ///
    /// note: the same rows the permissions tab draws, out of the same [`App::permissions`], and
    /// the same [`text::NOTHING_DECIDED`] where there are none. What a headless run is given is
    /// this page rather than the tab, because a tab is where somebody *changes* a rule and a
    /// caller with no keys changes nothing - but what it may have asked is what the rules are, and
    /// answering that with silence was the same answer as a policy that allows nothing.
    ///
    /// note: the tab is opened as well as the page. A person at a desk sent `/permissions` to be able
    /// to change a rule, and a page printed down a pipe of somebody else's session changes
    /// nothing there.
    ///
    /// note: no clip and no column widths, because there is no window here to fit. What is worth
    /// saying of a rule is its subject, its answer and what it covers, and all three are said in
    /// full - a row cut at some width a script's terminal happens to be would be a rule whose
    /// coverage reads as less than it is.
    pub(super) fn permissions_page(&mut self) {
        self.show(Tab::Permissions);
        // note: the page only where there is no tab to read it off, which is the same line
        // `/help` draws. Somebody at a desk sent this to be *on* the tab and to change a rule
        // there, and a panel floating over the tab they were sent to would be the opposite of
        // that - while a caller with no screen is sent a page or is sent silence
        if self.keys {
            return;
        }

        let rows = self.permissions();
        let undecided = self.undecided();
        let mut body = vec![format!(
            "{} · anything it has not been told about: {}",
            self.policy_name(),
            verdict_word(crate::tools::Careful::untold()),
        )];
        if rows.is_empty() {
            body.push(String::new());
            body.push(NOTHING_DECIDED.to_owned());
        } else {
            for row in rows {
                body.push(format!(
                    "  {}  {}  {}",
                    row.subject,
                    verdict_word(row.verdict),
                    row.covers()
                ));
            }
        }
        // note: counted here as well as on the tab, because the tab's count is drawn along its
        // bottom edge and a page has no bottom edge. It is the one figure that keeps a short
        // table from reading as a whole policy: these are the subjects a call will stop and ask
        // about, and a table of two decisions silently standing for sixteen answers is a different
        // kind of dishonest
        body.push(String::new());
        body.push(format!(
            "and {} it will ask about, which are not listed above",
            plural(undecided, "subject"),
        ));

        self.preview("the policy", body.join("\n"));
    }

    /// Says what the session has been charged, and changes the ceiling that stops it.
    ///
    /// note: named `spend_command` because `App::spend` is the ceiling itself and a method may not
    /// be both. The command is `/spend`, which is the word on the command line too.
    ///
    /// note: it exists because a ceiling a session cannot see is a session that stops for no
    /// reason anybody at it can read, and because somebody who set one and meant to set a larger
    /// one should not have to start again. `0` takes it away, the way `--requests 0` does - the
    /// same spelling for the same idea, rather than a second word for "none".
    ///
    /// note: every sentence here says the figure is *this run's*, because a total starts at
    /// nothing in every process and reads as the session's to anybody carried on with `-r` - and
    /// `0` on a resumed run is the case that misleads, being right and answering a question
    /// nobody asked. `/budget`'s count of what the counter learned is a different figure on a
    /// different basis, and the note beside that one says which is which.
    pub(super) fn spend_command(&mut self, rest: &str) {
        let spent = thousands(self.spent() as usize);
        if !rest.trim().is_empty() {
            let Ok(tokens) = rest.trim().parse::<u64>() else {
                self.say(
                    Speaker::Error,
                    format!("`{}` is not a number of tokens", rest.trim()),
                );
                return;
            };
            self.set_spend((tokens > 0).then_some(tokens));
            let said = match self.spend() {
                // a ceiling under what has already gone is a session that stops here, and saying
                // only what the new number is would leave somebody to find that out by being
                // refused. It is a legitimate thing to want - one way to stop a session is to tell
                // it that it has spent enough - so it is answered rather than argued with
                Some(limit) if self.overspent() => format!(
                    "the ceiling is {} and this run has spent {spent}, so nothing more will be \
                     sent",
                    plural(limit as usize, "token")
                ),
                Some(limit) => format!(
                    "the ceiling is {}; this run has spent {spent}",
                    plural(limit as usize, "token")
                ),
                None => format!("no ceiling; this run has spent {spent} tokens"),
            };
            self.say(Speaker::Note, said);
            self.say_advice_spent();

            return;
        }

        let said = match self.spend() {
            Some(limit) => format!(
                "this run has spent {spent} tokens of {}, as the provider has reported them. \
                 `/spend N` changes the ceiling and `/spend 0` takes it away",
                thousands(limit as usize)
            ),
            None => format!(
                "this run has spent {spent} tokens, as the provider has reported them. There is \
                 no ceiling; `/spend N` sets one, and the session stops when it is reached"
            ),
        };
        self.say(Speaker::Note, said);
        self.say_advice_spent();
    }

    /// Says how much of what `/spend` just reported was the advisor's, where any was.
    ///
    /// note: a line of its own rather than a clause in each of the sentences above, and only when
    /// there is something to say. The figure is in the total whichever key paid for it, and a
    /// person whose advisor has a key of its own is owed the split: what the model spent is
    /// otherwise not a number they can read off this.
    pub(super) fn say_advice_spent(&mut self) {
        let advice = self.spent_on_advice();
        if advice > 0 {
            self.say(
                Speaker::Note,
                format!(
                    "{} of it the advisor's, for rating commands",
                    plural(advice as usize, "token")
                ),
            );
        }
    }
}
