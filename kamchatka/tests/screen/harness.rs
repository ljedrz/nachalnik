//! The terminal these tests sit at.
//!
//! note: one harness rather than a builder per test file, because every one of them wants the
//! same thing: a kernel with a scripted model behind it, a key to press, and the characters that
//! came out. Anything that reads the screen as something other than a string - a colour, a
//! modifier, which dot is lit - is a method here, so that a test asserts on what it means rather
//! than on a buffer index.

use std::{sync::Arc, time::Duration};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::{
    app::{App, Focus, Outcome, Tab},
    provider::OpenAiCompatible,
    tools::{Careful, Limits},
    ui,
};
use nachalnik::{Config, Event, Kernel, ModelResponse, test::ScriptedProvider};
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
};
use tokio::sync::{broadcast::Receiver, mpsc::UnboundedReceiver};

/// A terminal, and everything needed to pretend somebody is sitting at it.
pub(crate) struct Harness {
    pub(crate) app: App,
    pub(crate) events: Receiver<Event>,
    pub(crate) finished: UnboundedReceiver<Outcome>,
}

impl Harness {
    /// Builds one around a model that will answer with exactly these.
    pub(crate) fn new(script: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self::configured(script, Config::default())
    }

    /// The same, over an endpoint that has already been asked what the model takes - so that the
    /// two ways `/params` can word an unlisted parameter are both reachable from here.
    pub(crate) fn served_by(
        script: impl IntoIterator<Item = ModelResponse>,
        provider: Arc<OpenAiCompatible>,
    ) -> Self {
        let mut harness = Self::configured(script, Config::default());
        // both, and the same one, because that is what `main` does: `/params` reads what the
        // *kernel* was told about the model and words it by what the endpoint says it published,
        // so a test holding two different providers would be testing a program nobody runs
        harness.app.kernel.set_provider(provider.clone());
        harness.app.provider = provider;
        harness
    }

    /// The same, for a runtime configured some other way.
    pub(crate) fn configured(
        script: impl IntoIterator<Item = ModelResponse>,
        config: Config,
    ) -> Self {
        let kernel = Kernel::new(config);
        // subscribed first, the way the program does it: plugging things in is itself a handful of
        // events, and they belong on the trace with the rest
        let events = kernel.subscribe();

        let policy = Arc::new(Careful::new());
        kernel.set_provider(Arc::new(ScriptedProvider::new(script)));
        kernel.set_policy(policy.clone());
        let (outcomes, finished) = tokio::sync::mpsc::unbounded_channel();
        // the screen never talks to this one; it is here for `/model`, which the tests do not use
        let provider = Arc::new(OpenAiCompatible::new("scripted", "http://127.0.0.1:1", ""));

        Self {
            app: App::new(kernel, policy, provider, Limits::default(), outcomes),
            events,
            finished,
        }
    }

    /// Answers the question standing in the prompt's place, moving the keys to it first.
    ///
    /// note: a question does not take the keys on arrival - see the note on `App::question_key`,
    /// and the live session that granted `shell` for good with the `a` of "what". Reaching it is
    /// `tab`, or coming back to the chat tab while it waits; a `y` pressed at the prompt instead
    /// is a `y` typed into a message, which is exactly what this saves every caller from writing.
    pub(crate) async fn answer(&mut self, code: KeyCode) {
        match self.app.tab {
            Tab::Chat if self.app.focus != Focus::Body => self.press(KeyCode::Tab).await,
            Tab::Chat => {}
            _ => self.app.show(Tab::Chat),
        }
        self.press(code).await;
    }

    /// Presses a key.
    pub(crate) async fn press(&mut self, code: KeyCode) {
        self.app
            .on_key(KeyEvent::new(code, KeyModifiers::NONE))
            .await;
    }

    /// Presses a key with control held.
    pub(crate) async fn chord(&mut self, code: KeyCode) {
        self.app
            .on_key(KeyEvent::new(code, KeyModifiers::CONTROL))
            .await;
    }

    /// Types a line and sends it.
    pub(crate) async fn send(&mut self, line: &str) {
        for c in line.chars() {
            self.press(KeyCode::Char(c)).await;
        }
        self.press(KeyCode::Enter).await;
    }

    /// Feeds the app whatever the kernel has broadcast since last time.
    pub(crate) fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.app.on_event(event);
        }
    }

    /// Opens a tab, the way `alt+2` would.
    pub(crate) fn tab(&mut self, tab: Tab) {
        self.app.show(tab);
    }

    /// Waits for the running turn to stop, the way the real loop does.
    ///
    /// note: With a deadline, because the alternative is a test that hangs for ever when a key
    /// press went somewhere other than where it was expected to - which is exactly the mistake
    /// this file exists to catch.
    pub(crate) async fn settle(&mut self) {
        let outcome = tokio::time::timeout(Duration::from_secs(5), self.finished.recv())
            .await
            .expect("a turn should have been started, and should have finished")
            .expect("the channel outlives the turn");

        self.drain();
        self.app.on_outcome(outcome);
    }

    /// Draws, and returns what is on the screen.
    pub(crate) fn screen(&mut self) -> String {
        self.sized(100, 30)
    }

    /// Draws, and reports how the first character of `needle` is styled.
    pub(crate) fn style_of(&mut self, needle: &str) -> (Color, Modifier) {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let first = needle.chars().next().expect("a needle to look for");
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(at) = row.find(needle) {
                // `find` gives bytes and the buffer is indexed in cells; every character this is
                // used on is one cell wide
                let x = row[..at].chars().count() as u16;
                let cell = &buffer[(x, y)];
                assert_eq!(
                    cell.symbol(),
                    first.to_string(),
                    "the cell under the needle"
                );

                return (cell.fg, cell.modifier);
            }
        }

        panic!("`{needle}` is not on the screen");
    }

    /// The screen with its line breaks undone: borders dropped, rows run together, runs of
    /// spaces collapsed to one.
    ///
    /// note: for asserting on a *sentence* rather than on a line. A phrase that sits comfortably
    /// on one row here wraps onto two on a machine whose temp directory is
    /// `/var/folders/df/djsxfhc17x95674wsm_g8s980000gn/T` - which is macOS, and which is how
    /// `is not a session` became `is not a` and then `session:` and a green test went red on
    /// somebody else's CI. Anything whose text can contain a path belongs here rather than in
    /// `screen`.
    pub(crate) fn flat(&mut self) -> String {
        let screen = self.screen();

        screen
            .replace(['│', '┌', '┐', '└', '┘', '─'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The same again, with the spaces taken out too.
    ///
    /// note: for finding a *token* rather than a sentence. `wrapped` splits a chunk that will not
    /// fit on a row of its own, so a path longer than the window is wide arrives as `…/jun` and
    /// `k.json` and no amount of running the rows together puts it back. This is the view that
    /// can still find it.
    pub(crate) fn packed(&mut self) -> String {
        self.flat().replace(' ', "")
    }

    /// Which of the three working dots is the lit one.
    pub(crate) fn dots(&mut self) -> usize {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let row = buffer.area.height - 1;
        let dots: Vec<u16> = (0..buffer.area.width)
            .filter(|x| buffer[(*x, row)].symbol() == "•")
            .collect();
        assert_eq!(dots.len(), 3, "three dots, or none at all");

        dots.iter()
            .position(|x| buffer[(*x, row)].fg == Color::Yellow)
            .expect("one of them is lit")
    }

    /// The colour the named tab is written in on the strip along the top.
    pub(crate) fn tab_colour(&mut self, name: &str) -> Color {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect();
        let at = row.find(name).expect("the tab is on the strip") as u16;

        buffer[(at, 0)].fg
    }

    /// The colour of each box's top-left corner, top to bottom: the window, and then the prompt
    /// or the question boxed under it.
    ///
    /// note: the corner rather than the title, because the title is what a box says and the border
    /// is what it is. One of them is drawn in the colour that means the keys are here.
    pub(crate) fn corners(&mut self) -> Vec<Color> {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        (0..buffer.area.height)
            .filter_map(|y| {
                (0..buffer.area.width)
                    .find(|x| buffer[(*x, y)].symbol() == "┌")
                    .map(|x| buffer[(x, y)].fg)
            })
            .collect()
    }

    /// Draws at a given size.
    pub(crate) fn sized(&mut self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
