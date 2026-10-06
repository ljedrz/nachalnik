//! Cutting a projection down to one frame: the longest lines first, to the longest length that
//! lets the whole of it fit, measured the way serde_json writes it.

use serde::Serialize;

use super::{Attached, MAX_LINE};

/// What a frame carrying a projection adds to the projection itself: `"is":"projected",`, and room
/// to spare.
const ENVELOPE: usize = 32;

/// What a line that is cut grows by: `"clipped":null` becoming `"clipped":` and a count, which is
/// at most twenty digits.
const CLIPPING: usize = 20;

impl Attached {
    /// Cuts the longest lines down until the projection fits in one frame, and leaves a projection
    /// that already fits exactly as it is.
    ///
    /// note: the decision `POSTPONED.md` once held, and it is a *derived* length rather than a
    /// chosen one. Any fixed length per line is either short enough to cut an ordinary long answer
    /// in a session that never needed it, or long enough that ten thousand lines of it go past
    /// the frame anyway - a projection is one message however many lines are in it. So nothing is
    /// cut until something has to be, and then the length is the longest one at which every line
    /// cut to it leaves the whole projection inside [`MAX_LINE`]: the lines shorter than that are
    /// left whole, and only the ones that are the problem are touched.
    ///
    /// note: measured as JSON rather than as text, because the frame is JSON and a message of
    /// quotes and newlines is up to twice its length on the wire, and one of control characters
    /// six times. [`escaped`] is serde_json's own arithmetic, so the cut is exact and there is no
    /// second attempt.
    ///
    /// note: what it cannot help is a projection too long for reasons other than its lines - a
    /// trace, a list of items - and it leaves that one as it is. The write that follows is what
    /// says so, because it measures every frame it sends.
    ///
    /// note: a cost worth knowing, and the reason it is stated here: whether a line arrives whole
    /// depends on how long the whole session is, so a message one projection carried whole can be
    /// cut in the next once the session has grown. Every client here takes each projection as the
    /// conversation afresh, so nothing is left disagreeing, and the line says what it lost.
    pub(crate) fn abridge(&mut self) {
        self.abridge_within(MAX_LINE);
    }

    /// The same, inside a frame of any length.
    ///
    /// note: apart so that what it promises can be held to it at a length a test can afford. At
    /// [`MAX_LINE`] one case is thirty-two megabytes, and the cases worth having are hundreds.
    fn abridge_within(&mut self, limit: usize) {
        let whole = measured(self) + ENVELOPE;
        if whole <= limit {
            return;
        }
        let sizes: Vec<usize> = self
            .conversation
            .iter()
            .map(|line| line.text.as_str())
            .chain(self.queued.as_deref())
            .chain(self.queued_behind.iter().map(String::as_str))
            .map(escaped)
            .collect();
        let fixed = whole - sizes.iter().sum::<usize>();
        let Some(room) = limit.checked_sub(fixed + CLIPPING * sizes.len()) else {
            return;
        };
        let cap = level(&sizes, room);

        for line in &mut self.conversation {
            line.clipped = cut(&mut line.text, cap);
        }
        for text in self.queued.iter_mut().chain(&mut self.queued_behind) {
            cut(text, cap);
        }
    }
}

/// How long a value is as JSON, without writing it anywhere.
fn measured(value: &impl Serialize) -> usize {
    struct Counted(usize);

    impl std::io::Write for Counted {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counted = Counted(0);
    // a value that cannot be serialized cannot be framed either, and the write says so
    let _ = serde_json::to_writer(&mut counted, value);

    counted.0
}

/// How long one character is inside a JSON string, the way serde_json writes it.
fn escaped_char(c: char) -> usize {
    match c {
        '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
        '\0'..='\u{1f}' => 6,
        _ => c.len_utf8(),
    }
}

/// How long a text is inside a JSON string, not counting the quotes around it.
fn escaped(text: &str) -> usize {
    text.chars().map(escaped_char).sum()
}

/// The longest length every size can be held to and still come to no more than `room` in all.
///
/// note: the shortest first, because each one that fits whole hands what it did not use to the
/// ones still over. Where they all fit there is no length to hold them to, which is what `MAX`
/// says.
fn level(sizes: &[usize], room: usize) -> usize {
    let mut sorted = sizes.to_vec();
    sorted.sort_unstable();
    let mut left = room;
    for (at, &size) in sorted.iter().enumerate() {
        let share = left / (sorted.len() - at);
        if size > share {
            return share;
        }
        left -= size;
    }

    usize::MAX
}

/// Cuts a text to the longest start of it that is no more than `cap` long as JSON, and says how
/// many bytes went; `None` where it was short enough already.
fn cut(text: &mut String, cap: usize) -> Option<usize> {
    if escaped(text) <= cap {
        return None;
    }
    let mut used = 0;
    let mut end = 0;
    for (at, c) in text.char_indices() {
        used += escaped_char(c);
        if used > cap {
            break;
        }
        end = at + c.len_utf8();
    }
    let gone = text.len() - end;
    text.truncate(end);

    Some(gone)
}

#[cfg(test)]
mod tests {
    use nachalnik::{Budget, ContextId, State};
    use proptest::prelude::*;

    use super::*;
    use crate::{
        app::Speaker,
        remote::protocol::{Line, VERSION},
    };

    /// The arithmetic a projection is cut by is serde_json's own, character for character.
    ///
    /// note: what the cut is exact *because of*. Measured as text rather than as JSON, a message
    /// of quotes and newlines comes out twice the length it was cut to and one of control
    /// characters six times, and the projection it was cut to fit goes past the frame anyway.
    #[test]
    fn a_text_is_measured_the_way_serde_json_writes_it() {
        let text = "plain, \"quoted\", back\\slash, a\ttab\nand\rbreaks, \u{0}\u{1f}\u{7f}, \
                    ünïcödé, 漢字, 🦀, and a / that is left alone";
        let written = serde_json::to_string(text).unwrap();
        assert_eq!(escaped(text), written.len() - 2);
        for c in text.chars() {
            let written = serde_json::to_string(&c.to_string()).unwrap();
            assert_eq!(escaped_char(c), written.len() - 2, "{c:?}");
        }
    }

    /// The lines under the length are left whole and hand what they did not use to the ones over it.
    #[test]
    fn a_length_is_the_longest_that_leaves_everything_inside_the_room() {
        // the 1 and the 10 fit whole and leave 50 of the 61, which is what the 100 is held to
        assert_eq!(level(&[100, 1, 10], 61), 50);
        // in any order, because it is the sizes that decide and not where they are
        assert_eq!(level(&[10, 100, 1], 61), 50);
        // two over it share what is left between them
        assert_eq!(level(&[100, 100, 1], 61), 30);
        // and where everything fits there is nothing to hold anything to
        assert_eq!(level(&[1, 10], 61), usize::MAX);
        // and where nothing does, nothing is what each gets
        assert_eq!(level(&[5, 5], 1), 0);
    }

    /// A text is cut on a character, to no more than the length as JSON, and says what it lost.
    #[test]
    fn a_text_is_cut_to_its_length_as_json_on_a_character() {
        let mut short = "short".to_owned();
        assert_eq!(cut(&mut short, 5), None);
        assert_eq!(short, "short");

        // `"` is two as JSON, so four of it is the first two
        let mut quoted = "\"\"\"\"".to_owned();
        assert_eq!(cut(&mut quoted, 4), Some(2));
        assert_eq!(quoted, "\"\"");

        // and a character is not split: `ü` is two bytes, and a length of four keeps `aü` whole
        // and not one byte of the second `ü`
        let mut letters = "aüü".to_owned();
        assert_eq!(cut(&mut letters, 4), Some(2));
        assert_eq!(letters, "aü");
        let mut letters = "aüü".to_owned();
        assert_eq!(cut(&mut letters, 2), Some(4));
        assert_eq!(letters, "a");
    }

    /// Any text, with the characters that measure differently as JSON in it often enough to
    /// matter: the two-byte escapes, the six-byte ones, and characters of two, three and four bytes.
    fn any_text(longest: usize) -> impl Strategy<Value = String> {
        proptest::string::string_regex(&format!("[a-z \"\\\\\n\t\u{1}\u{1f}é漢🦀]{{0,{longest}}}"))
            .expect("a pattern for the text")
    }

    /// `level` is the longest length that fits, and not merely one that does.
    #[test]
    fn the_length_is_the_longest_that_fits() {
        proptest!(
            ProptestConfig {
                cases: 512,
                failure_persistence: None,
                ..ProptestConfig::default()
            },
            |(sizes in prop::collection::vec(0usize..500, 0..12), room in 0usize..3000)| {
                let held = |cap: usize| sizes.iter().map(|&size| size.min(cap)).sum::<usize>();
                let cap = level(&sizes, room);
                prop_assert!(held(cap) <= room, "{cap} does not fit in {room}");
                if cap == usize::MAX {
                    prop_assert!(sizes.iter().sum::<usize>() <= room);
                } else {
                    prop_assert!(held(cap + 1) > room, "{} fits in {room} as well", cap + 1);
                }
            }
        );
    }

    /// `cut` keeps the longest start of a text that is no longer than the length as JSON.
    #[test]
    fn a_cut_is_the_longest_start_that_fits() {
        proptest!(
            ProptestConfig {
                cases: 512,
                failure_persistence: None,
                ..ProptestConfig::default()
            },
            |(text in any_text(60), cap in 0usize..200)| {
                let mut kept = text.clone();
                let gone = cut(&mut kept, cap);
                prop_assert!(text.starts_with(&kept));
                prop_assert!(escaped(&kept) <= cap);
                match gone {
                    None => prop_assert_eq!(&kept, &text),
                    Some(gone) => {
                        prop_assert_eq!(kept.len() + gone, text.len());
                        // and one more character would have been one too many
                        let next = text[kept.len()..].chars().next().expect("nothing was cut");
                        prop_assert!(escaped(&kept) + escaped_char(next) > cap);
                    }
                }
            }
        );
    }

    /// A projection that fits is left exactly as it is, one that does not is cut to fit and no
    /// further, the lines cut are the longest ones, and each says what it lost.
    ///
    /// note: inside frames of a few kilobytes rather than [`MAX_LINE`], which is the one thing
    /// `abridge_within` is apart for: the arithmetic is the same at any length, and at the real one
    /// a case is thirty-two megabytes.
    ///
    /// note: the messages still waiting are generated as the last lines of the conversation as
    /// well, because that is what they are in a real projection - and the claim on
    /// [`Attached::queued`] is that the two are cut the same way.
    ///
    /// note: what it reached is counted and held to, so that a property over three outcomes
    /// cannot quietly become one over the easy one.
    #[test]
    fn a_projection_is_cut_to_fit_and_no_further() {
        #[derive(Default, Debug)]
        struct Reached {
            untouched: usize,
            cut: usize,
            gave_up: usize,
            cut_an_escape: usize,
        }

        let reached = std::cell::RefCell::new(Reached::default());
        let lines = prop::collection::vec(
            (
                prop_oneof![3 => any_text(40), 2 => any_text(1500)],
                any::<bool>(),
            ),
            0..10,
        );
        let waiting = prop::collection::vec(any_text(600), 0..3);

        proptest!(
            ProptestConfig {
                cases: 512,
                failure_persistence: None,
                ..ProptestConfig::default()
            },
            |(lines in lines, waiting in waiting, fixed in 0usize..2000, limit in 300usize..6000)| {
                let mut conversation: Vec<Line> = lines
                    .iter()
                    .enumerate()
                    .map(|(at, (text, is_item))| Line {
                        speaker: Speaker::Model,
                        text: text.clone(),
                        item: is_item.then_some(ContextId(at as u64)),
                        clipped: None,
                    })
                    .collect();
                conversation.extend(waiting.iter().map(|text| Line {
                    speaker: Speaker::User,
                    text: text.clone(),
                    item: None,
                    clipped: None,
                }));
                let before = projection(conversation, &waiting, fixed);
                let mut after = before.clone();
                after.abridge_within(limit);
                let mut reached = reached.borrow_mut();

                let n = after.conversation.len() + waiting.len();
                let fits = measured(&after) + ENVELOPE <= limit;
                if measured(&before) + ENVELOPE <= limit {
                    reached.untouched += 1;
                    prop_assert_eq!(&after, &before, "a projection that fits was changed");
                    return Ok(());
                }
                if !fits {
                    // given up on, and only where even every line cut to nothing would not fit
                    reached.gave_up += 1;
                    prop_assert_eq!(&after, &before, "a projection given up on was changed");
                    let mut emptied = before.clone();
                    for line in &mut emptied.conversation {
                        line.text.clear();
                    }
                    emptied.queued = emptied.queued.map(|_| String::new());
                    emptied.queued_behind.iter_mut().for_each(String::clear);
                    prop_assert!(
                        measured(&emptied) + ENVELOPE + CLIPPING * n > limit,
                        "it gave up on a projection it could have cut to fit"
                    );
                    return Ok(());
                }
                reached.cut += 1;

                let mut untouched = Vec::new();
                let mut cut = Vec::new();
                for (was, line) in before.conversation.iter().zip(&after.conversation) {
                    prop_assert!(was.text.starts_with(&line.text), "a line is not its own start");
                    prop_assert_eq!(line.item, was.item);
                    match line.clipped {
                        None => {
                            prop_assert_eq!(&line.text, &was.text);
                            untouched.push(escaped(&was.text));
                        }
                        Some(gone) => {
                            prop_assert_eq!(line.text.len() + gone, was.text.len());
                            prop_assert!(gone > 0, "a line says it was cut by nothing");
                            cut.push((escaped(&was.text), escaped(&line.text)));
                            if was.text.contains(['"', '\\', '\n', '\t', '\u{1}', '\u{1f}']) {
                                reached.cut_an_escape += 1;
                            }
                        }
                    }
                }
                // the lines cut are the longest ones: every line left whole was shorter than every
                // line that was cut, and the cut ones end within a character of each other
                if let (Some(&longest_whole), Some(&(shortest_cut, _))) =
                    (untouched.iter().max(), cut.iter().min())
                {
                    prop_assert!(longest_whole < shortest_cut, "{longest_whole} whole, {shortest_cut} cut");
                }
                let ends = cut.iter().map(|&(_, now)| now);
                if let (Some(low), Some(high)) = (ends.clone().min(), ends.max()) {
                    prop_assert!(high - low < 6, "cut to {low} and to {high}");
                }
                // the messages waiting are cut exactly as their lines in the conversation were
                let tail = &after.conversation[after.conversation.len() - waiting.len()..];
                let waited: Vec<&str> = after
                    .queued
                    .iter()
                    .chain(&after.queued_behind)
                    .map(String::as_str)
                    .collect();
                let lined: Vec<&str> = tail.iter().map(|line| line.text.as_str()).collect();
                prop_assert_eq!(waited, lined);
                // the room kept for each count is room for any count, which these cases are far
                // too short to reach on their own: a line of thirty megabytes cut to one says so in
                // eight digits where `null` took four
                let mut longest_counts = after.clone();
                for line in &mut longest_counts.conversation {
                    if line.clipped.is_some() {
                        line.clipped = Some(usize::MAX);
                    }
                }
                prop_assert!(
                    measured(&longest_counts) + ENVELOPE <= limit,
                    "the counts outgrew the room kept for them"
                );
                // and no further than it had to: what is left over is the rounding, a character a
                // line, and the room kept for each count - not a length chosen short to be safe
                let left = limit - (measured(&after) + ENVELOPE);
                prop_assert!(left <= 29 * n, "{left} bytes to spare across {n} line(s)");
            }
        );

        let reached = reached.into_inner();
        assert!(reached.untouched > 20, "{reached:?}");
        assert!(reached.cut > 50, "{reached:?}");
        assert!(reached.gave_up > 5, "{reached:?}");
        assert!(reached.cut_an_escape > 20, "{reached:?}");
    }

    /// A projection carrying the given conversation and messages waiting, and `fixed` bytes of
    /// what nothing cuts.
    fn projection(conversation: Vec<Line>, waiting: &[String], fixed: usize) -> Attached {
        Attached {
            version: VERSION,
            session: "abridged".to_owned(),
            seq: 0,
            state: State::Idle,
            busy: false,
            stepping: false,
            model: None,
            budget: Budget {
                context_tokens: 0,
                tool_tokens: 0,
                uncounted: 0,
                limit: None,
                reported: None,
            },
            spent: 0,
            spend: None,
            overspent: false,
            conversation,
            items: Vec::new(),
            asking: Vec::new(),
            reaching: Vec::new(),
            rated: Vec::new(),
            unrated: Vec::new(),
            policy: "p".repeat(fixed),
            untold: nachalnik::Verdict::Ask,
            permissions: Vec::new(),
            trace: Vec::new(),
            undecided: 0,
            queued: waiting.first().cloned(),
            queued_behind: waiting.iter().skip(1).cloned().collect(),
            confinement: None,
        }
    }
}
