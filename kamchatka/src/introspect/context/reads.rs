//! The half of `context` that only reads: the listing, the search, the budget and the request.

use std::{collections::BTreeSet, sync::Arc};

use nachalnik::{Block, Content, ContextId, ContextItem, ContextKind, Event, Kernel, ToolCall};
use serde_json::Value;

use crate::{
    app::{Going, text::thousands},
    introspect::{Mine, TAKE, protected},
    tools::Careful,
};

/// How much of an item's text the listing shows on its row.
const GLIMPSE: usize = 48;

/// How many matching lines a `search` was asked for, or what is wrong with the way it asked.
///
/// note: a `take` that cannot be read is refused, as `log` refuses its own, so the same word does
/// not mean two things a tool apart. Read with a bare `as_u64`, `take: 0` would print a heading
/// with a colon and nothing under it, which is a malformed answer rather than a wrong one; and
/// `take: -3` and `take: "3"` would be `None`, which is the summary that leaving `take` out gives:
/// the model asked for lines, was given a count, and nothing said its argument had not been read.
pub(super) fn taken(args: &Value) -> Result<Option<usize>, String> {
    match crate::tools::number(args, "take") {
        // `0` is not an error, because it is a coherent thing to have asked for and the tool has
        // an answer to it already: the count and the price, which is what a call with no `take`
        // gets. Refusing it would spend a turn on a call that meant something
        Ok(take) => Ok(take.filter(|&take| take != 0).map(|take| take as usize)),
        Err(_) => Err(format!(
            "`take` is a whole number of lines and this one is `{}`. Nothing was read, rather \
             than nothing being found: leave it out for the count and the price, which is what \
             `take: 0` asks for too.",
            args["take"]
        )),
    }
}

/// The context, item by item, or the whole of the named ones.
///
/// note: two columns of figures rather than one headed `tokens`, and they are the two the person's
/// own pane shows: what an item puts into the next request, and what it is keeping out of one.
/// One column could only be one of those, and whichever it was would be wrong about the rows that
/// matter - an elided item, which sends a marker and holds its content, and any turn whose
/// thinking the endpoint will not take back, which is every turn under an OpenAI-compatible one.
/// Read as a budget, the held figure invites giving up what the request was not carrying; read as
/// an inventory, the sending figure hides tens of thousands of tokens the agent really is
/// carrying. Both, named, is the only honest answer, and it is what `held` in the next line and
/// the expensive list under `budget` are counted from.
pub(super) fn look(
    kernel: &Kernel,
    ids: &[ContextId],
    whole: bool,
    own: Option<ContextId>,
    mine: &Mine,
    policy: &Careful,
) -> String {
    let items = kernel.items();
    let going = Going::of(kernel);
    if !ids.is_empty() {
        return ids
            .iter()
            .map(|id| full(&items, *id, &going, whole, mine, own))
            .collect::<Vec<_>>()
            .join("\n");
    }

    let budget = kernel.budget();
    // note: the undo depth is reported and named as somebody else's on purpose. It is the stack
    // behind the `u` key in the terminal, it holds the latest changes to this context by
    // anybody, as many as `Config::context_undo_depth` keeps, and this tool's own `undo` does not
    // touch it - that figure, sitting unlabelled beside an operation called `undo`, would be an
    // invitation to try to walk back the person's work
    let theirs = kernel.with_context(|context| context.undo_len());
    let withheld: usize = items.iter().map(|item| going.held_back(item)).sum();

    let mut out = inherited(kernel, &items, policy);
    out.push_str(&format!(
        "{} items · {} of them go into the next request\n\
         ~{} tokens going{}, ~{} held back - what the request does not carry, whether because \
         you set a state or because the endpoint will not take it\n\
         {} change(s) in the person's own undo stack, which is theirs; `undo` walks back what \
         you did\n\n\
         {HEAD}",
        items.len(),
        // what the projection carries, and the turn this call is in, which it will carry once
        // this call has an answer
        items
            .iter()
            .filter(|item| going.costs.contains_key(&item.id) || Some(item.id) == own)
            .count(),
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(
                " of {} ({}%)",
                thousands(limit),
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize
            ))
            .unwrap_or_default(),
        thousands(withheld),
        theirs,
    ));

    for item in &items {
        out.push_str(&line(item, &going, &row(item, &going)));
    }
    out.push_str(floor(&items));

    // note: the second sentence is there because the columns are easy to read wrong. Asked what
    // it could free, a model names the turn holding the most of its own thinking and offers to
    // elide it - which frees what that turn is *sending* and not one token of what it holds,
    // because the endpoint is never sent those. `budget` says this under its own table, and
    // `look` is where an agent actually reads the figure
    out.push_str(
        "\n`look` with `ids` reads any of these back, including the reasoning recorded on an \
         assistant turn; a long one arrives as its start and its end unless you ask for the \
         `whole` of it. What a row shows under `held` is already out of the next request: giving \
         that item up frees what it is `sending` and none of what it is holding.\n",
    );

    out
}

/// The head of the table `look` lists items in, with a selector or without one.
const HEAD: &str = "  id  state       kind                 sending      held  what it is\n";

/// One row of that table, ending in what the item is.
fn line(item: &ContextItem, going: &Going, said: &str) -> String {
    format!(
        "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {said}\n",
        item.id.0,
        item.state.to_string(),
        item.kind.name(),
        figure(item, going),
        match going.held_back(item) {
            0 => String::new(),
            held => thousands(held),
        },
    )
}

/// What one row says an item is sending: its figure, with a `+` where part of it is unpriced.
///
/// note: marked as the context tab marks it, so that the two things `0` can mean are not one
/// figure. One function for both tables, because a `look` narrowed by a selector is the same
/// listing and a picture in it is no cheaper there.
fn figure(item: &ContextItem, going: &Going) -> String {
    let figure = thousands(going.costs.get(&item.id).copied().unwrap_or(0));
    match item.uncounted {
        0 => figure,
        _ => format!("{figure}+"),
    }
}

/// The line under a table that says what its `+` means, where any row carries one.
fn floor(items: &[Arc<ContextItem>]) -> &'static str {
    match items.iter().any(|item| item.uncounted != 0) {
        true => {
            "\na `+` is a floor: part of that item is content nothing here can put a number on, \
             so it and the total going cost more than they say\n"
        }
        false => "",
    }
}

/// The rows of the items a class comes to, which is the set a change naming the same class takes.
///
/// note: a selector is resolved against the context at the moment it is used, and without this the
/// only way to use one is to move something: `elide` with `select: "tool:shell"` says what it has
/// taken *after* taking it. Undoing that is one call, and knowing first is none.
///
/// note: the figures are the matched items' own rather than the session's, because that is the
/// number the decision turns on - what giving this class up would free - and the context's total
/// is on the line beside it to read them against. The rest of the accounting is `budget`'s.
///
/// note: the context's total and not the request's, which is what this line used to say. The two
/// figures are sums over the matched items and the request's total is a sum over every item plus
/// the tool definitions, which have no row in the table under it - so a header that put one beside
/// the other read as "this class is 1,531 of a 6,107 request, and 4,576 is somewhere else in it",
/// and a model budgeting against it concluded that the tool definitions were a quarter of a
/// context it cannot act on and that the four results it could give up were a quarter of what was
/// costing it. Both are wrong, in the one direction that makes giving things up look futile. The
/// definitions are named on the request's own line below, where they belong.
///
/// note: the items a change would refuse are marked here rather than left to be discovered by the
/// change. A preview that named four items where a move takes three is exactly the confident wrong
/// answer the rest of this tool is written to avoid, and [`protected`] is the same function the
/// move itself consults, so the two cannot come apart.
pub(super) fn matched(
    kernel: &Kernel,
    select: &str,
    ids: &[ContextId],
    mine: &Mine,
    own: Option<ContextId>,
    policy: &Careful,
) -> String {
    let items = kernel.items();
    let going = Going::of(kernel);
    let wanted: BTreeSet<ContextId> = ids.iter().copied().collect();
    let picked: Vec<&Arc<ContextItem>> =
        items.iter().filter(|it| wanted.contains(&it.id)).collect();
    if picked.is_empty() {
        return format!(
            "`{select}` is a selector, and nothing in your context matches it. `look` with no \
             `select` lists what there is.{}\n",
            crate::introspect::unmatched_file(kernel, select)
        );
    }

    let sending: usize = (picked.iter())
        .map(|item| going.costs.get(&item.id).copied().unwrap_or(0))
        .sum();
    let withheld: usize = picked.iter().map(|item| going.held_back(item)).sum();
    let carried: Vec<Arc<ContextItem>> = picked.iter().map(|item| Arc::clone(item)).collect();
    let budget = kernel.budget();

    let mut out = inherited(kernel, &carried, policy);
    out.push_str(&format!(
        "`{select}` matches {} of {} items · {} of them go into the next request\n\
         ~{} of the ~{} tokens in the context are those items, and ~{} more of what they hold is \
         not going into the request\n\
         the next request is ~{}, of which ~{} is the tool definitions\n\n\
         {HEAD}",
        picked.len(),
        items.len(),
        picked
            .iter()
            .filter(|item| going.costs.contains_key(&item.id) || Some(item.id) == own)
            .count(),
        thousands(sending),
        thousands(budget.context_tokens),
        thousands(withheld),
        thousands(budget.used()),
        thousands(budget.tool_tokens),
    ));

    let mut refused = 0;
    for item in &picked {
        let mut said = row(item, &going);
        if let Some(why) = protected(item, mine, own) {
            refused += 1;
            said.push_str(&format!(" · not yours to move: {why}"));
        }
        out.push_str(&line(item, &going, &said));
    }

    out.push_str(floor(&carried));
    out.push_str(&format!(
        "\na change naming the same `select` takes these{}. Giving them up frees what they are \
         `sending` and none of what they are holding; `look` with `ids` reads any of them back in \
         full.\n",
        match refused {
            0 => String::new(),
            n => format!(", less the {n} marked as not yours"),
        }
    ));

    out
}

/// Which of these items this session did not produce, said before the listing rather than after.
///
/// note: a session resumed under a *second* model, asked whether it wrote an inherited turn,
/// reaches for `look` - and without this line `look` has nothing to say, because an item restored
/// from a snapshot is a perfectly ordinary item with no field that marks it. The model reads the
/// turn, recognises its own voice in it, and confabulates a first-person account of writing a
/// sentence another model wrote.
///
/// note: `setup` with `model` says this too, and a model asking the question does not call it. A
/// fact that is only reachable by a tool nobody reaches for is a fact the program does not really
/// have, so it goes where the question is actually asked. It is a count of items in a listing that
/// already counts items - not a warning, and it says nothing about what the turns are worth.
///
/// note: conditioned on `session.resumed` being in the log rather than on the absence of a
/// `context.added` alone, because `Kernel::drain_history` also takes those away. Absent that
/// record, an item with no beginning here means the log was shortened, which is a different fact
/// and would be a false one to report as this.
fn inherited(kernel: &Kernel, items: &[Arc<ContextItem>], policy: &Careful) -> String {
    let (resumed, added) = kernel.with_history(|session| {
        let mut resumed = false;
        let mut added = BTreeSet::new();
        for record in session.records() {
            match &record.event {
                Event::SessionResumed { .. } => resumed = true,
                Event::ContextAdded { id, .. } => {
                    added.insert(*id);
                }
                _ => {}
            }
        }

        (resumed, added)
    });
    if !resumed {
        return String::new();
    }

    let carried: Vec<String> = items
        .iter()
        .filter(|item| !added.contains(&item.id))
        .map(|item| item.id.0.to_string())
        .collect();
    if carried.is_empty() {
        return String::new();
    }

    format!(
        "this session was resumed from a snapshot, and {} of the items below were already in it: \
         {}. They were not produced here. A turn among them reads in the first person and was \
         written by whatever model that session ran, which nothing in the turn records{}.\n\n",
        carried.len(),
        carried.join(", "),
        crate::introspect::if_reachable(kernel, policy, "setup:model", || {
            " - `setup` with `model` says what this one is".to_owned()
        }),
    )
}

/// Whether this session was resumed from a snapshot, which `session.resumed` in its log says.
pub(super) fn resumed(kernel: &Kernel) -> bool {
    kernel.with_history(|session| {
        session
            .records()
            .any(|record| matches!(record.event, Event::SessionResumed { .. }))
    })
}

/// One item's row: its label, then whatever else is worth knowing on one line.
fn row(item: &ContextItem, going: &Going) -> String {
    let glimpsed = glimpse(&item.content.to_text());
    let mut said = match glimpsed.is_empty() {
        true => item.label.clone(),
        false => format!("{}: {glimpsed}", item.label),
    };
    if matches!(item.kind, ContextKind::AssistantMessage { .. }) {
        // `calls()` and `thinking()` rather than the kind's own slots: a turn a provider recorded
        // in the order it was produced keeps both inside its content, and a row that read the
        // slots would report a reasoning model as having thought nothing and asked for nothing
        let calls = item.calls().count();
        if calls != 0 {
            said.push_str(&format!(" [{calls} call(s)]"));
        }
        if item.thinking().next().is_some() {
            said.push_str(" [+reasoning]");
        }
        if let Some(blocks) = item.content.as_blocks() {
            said.push_str(&format!(" [{} ordered block(s)]", blocks.len()));
        }
    }
    if let Some(note) = &item.note {
        said.push_str(&format!(" · {note}"));
    }
    // and why it is not in the request, where the state column cannot say. An item the projector
    // dropped is `active` and not going, so the row reads `0` under `sending` with nothing to
    // account for it - which is how a model sees its own latest turn: the turn carrying the call
    // being answered has no result yet, so it is out of the projection for as long as the tool
    // runs. The pane says this on the row, in the projector's own words, and this is the same
    // sentence out of the same place
    if let Some(why) = going.left_out.get(&item.id) {
        said.push_str(&format!(" · not going: {why}"));
    }

    said
}

/// How much of an item's content `look` shows either side of the gap before it is asked for the
/// whole thing.
///
/// note: reading an item copies that item into the context. So asking to see a 9,000-token tool
/// result in order to decide whether to keep it costs very nearly what keeping it costs, and a
/// clean-up done that way can finish heavier than it started. A head and a tail is enough to tell
/// build noise from something worth keeping, and the whole thing is still one argument away for
/// the times it is really wanted.
const SAMPLE: usize = 1_500;

/// An item's content: whole if it is small or if it was asked for, a head and a tail otherwise.
fn sampled(text: &str, whole: bool) -> String {
    if whole || text.len() <= SAMPLE * 2 {
        return text.to_owned();
    }

    // on a character boundary, so that a cut through a multi-byte character does not panic
    let mut head = SAMPLE;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - SAMPLE;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }

    format!(
        "{}\n[... {} bytes not shown. Asking for an item copies it into your context, so reading \
         all of this costs about what carrying it costs; `whole: true` if you need it anyway ...]\n{}",
        &text[..head],
        thousands(tail - head),
        &text[tail..],
    )
}

/// The whole of one item, or the fact that there is no such item.
fn full(
    items: &[Arc<ContextItem>],
    id: ContextId,
    going: &Going,
    whole: bool,
    mine: &Mine,
    own: Option<ContextId>,
) -> String {
    let Some(item) = items.iter().find(|item| item.id == id) else {
        return format!("[{id}] there is no such item\n");
    };

    // note: what it holds, and then how much of that the request does not carry - because this is
    // the view that reads a turn's *thinking* back, and under most endpoints the thinking is the
    // part that is not carried. A line reporting one figure would be telling the agent that the
    // page it is reading costs what it weighs, in the one place it is most likely to be wrong.
    //
    // note: `Going::holds` rather than `ContextItem::tokens`, so that the two figures in this one
    // sentence are counted on one scale - `Going::holds` has why that is not the same figure
    let mut out = format!(
        "[{}] {} · {} · from {} · {} · {} tokens{}{}\n",
        item.id,
        item.label,
        item.kind.name(),
        item.source,
        item.state,
        thousands(going.holds(item)),
        match item.uncounted {
            0 => String::new(),
            n => format!(" and {n} piece(s) nothing here can price"),
        },
        match going.held_back(item) {
            0 => String::new(),
            held => format!(
                ", {} of them held back from the next request",
                thousands(held)
            ),
        },
    );
    if let Some(because) = &item.included_because {
        out.push_str(&format!("  it is here because: {because}\n"));
    }
    if let Some(note) = &item.note {
        out.push_str(&format!("  it is {} because: {note}\n", item.state));
    }
    if !item.meta.is_null() {
        out.push_str(&format!("  attached: {}\n", item.meta));
    }
    // note: what the moves will say about it, beside whatever its own metadata says. The metadata
    // is a record of what was written when it was written - a pin this tool made, still reading
    // `by: context` on an item the person has since pinned again - and a model reading it that
    // way keeps believing the pin is its own and keeps being refused. The refusal itself is what
    // says whose it is, and this is the same [`protected`] the move consults, so the two cannot
    // come apart; the alternative was to leave the metadata out, which would hide a record
    // somebody may be asking for
    if let Some(why) = protected(item, mine, own) {
        out.push_str(&format!("  not yours to move: {why}\n"));
    }
    // a turn that was recorded as an order is read back as one, block by block. This is the
    // thing `context` exists for and the one view of it that is not available anywhere else: the
    // request the model will be sent has the same parts in the same order, but by then the
    // thinking looks like a field rather than something that happened between two calls
    if let Some(blocks) = item.content.as_blocks() {
        out.push_str(&format!("  --- {} block(s), in order ---\n", blocks.len()));
        for (at, block) in blocks.iter().enumerate() {
            let said = match block {
                Block::Call(call) => format!("{}({})", call.tool, call.args),
                _ => block
                    .part()
                    .map(|part| part.content.to_text().into_owned())
                    .unwrap_or_default(),
            };
            let signed = match block.extra().is_null() {
                true => "",
                false => " (signed)",
            };
            out.push_str(&format!("  [{at}] {}{signed}: {said}\n", block.name()));
        }

        return out;
    }

    // the reasoning first, because on the turn that carries it it is the part that explains the
    // rest, and because it is the one thing here the model cannot see in the request itself
    if let Some(reasoning) = item.reasoning() {
        out.push_str(&format!("  --- reasoning ---\n{}\n", reasoning.to_text()));
    }
    out.push_str(&format!(
        "  --- content ---\n{}\n",
        sampled(&item.content.to_text(), whole)
    ));

    out
}

/// The tools whose answers are about this session rather than about anything outside it.
///
/// note: the four this program installs to let a model look at itself. An answer from one of
/// them is a reading of the context, the record or the setup, so in the turn that asked for it
/// it can say back what the turn said - which is why a search leaves it out of that turn.
const ABOUT_THE_SESSION: [&str; 4] = ["context", "fork", "log", "setup"];

/// Whether this is what a tool said about something other than the session.
fn told_by_the_world(item: &ContextItem) -> bool {
    matches!(&item.kind, ContextKind::ToolResult { tool, .. } if !ABOUT_THE_SESSION.contains(&tool.as_str()))
}

/// Where a piece of text is in the context, and what reading it would cost - the count first.
///
/// note: the thing `look` cannot do. An excluded item is kept in full and never sent, and the only
/// other way to see inside one is to read it back, which copies it into the context - so a session
/// could not look at anything it had put away without undoing the saving. That would make what is
/// excluded write-only from the model's side, which is not what "nothing is destroyed" is supposed
/// to mean.
///
/// note: so the rule is `log`'s rule, for the same reason: the count and its price first, the
/// lines on request, and never the item. A search that answered with what it found would be a
/// second way to pay for an item without meaning to, which is the thing this action exists to
/// undo.
///
/// note: case is ignored. A model that searched for `landlock` and was told there are no matches
/// in a context full of `Landlock` has been told something false about itself, and the failure is
/// silent - which is the one shape of wrong answer a search must not have.
pub(super) fn search(
    kernel: &Kernel,
    text: &str,
    only: &[ContextId],
    take: Option<usize>,
    own: &BTreeSet<ContextId>,
) -> String {
    let needle = text.to_lowercase();
    let items = kernel.items();

    // (item, matching lines), in context order
    let mut found: Vec<(&Arc<ContextItem>, Vec<String>)> = Vec::new();
    // counted rather than assumed from `only`, because the two differ exactly where it matters:
    // an id naming nothing is looked at zero times, and reporting it as one looked at is this
    // tool saying an item exists and does not contain the text. `look` answers `[99] there is no
    // such item`; a search that quietly counted it would be the less honest of two siblings
    let mut read = 0;
    for item in &items {
        if !only.is_empty() && !only.contains(&item.id) {
            continue;
        }
        read += 1;
        // but not the turn asking. Its calls carry the text being searched for, and so does
        // whatever it said and thought on the way to making them - and so does the message that
        // started the turn, which is a context item of its own. So a search that read the rest of
        // the turn would find the needle in the question the model is in the middle of asking,
        // and report a match it made itself
        //
        // note: except what a tool answered in it, which nobody in the turn wrote. A model that
        // read a file and then searched it for a function the file defines was told no line of
        // its context said the name - the false "nothing" the note above calls the one wrong
        // answer - because the read was in the turn asking. What still goes unread is a report on
        // the session itself, which quotes the turn back: a search's own answer names the text it
        // looked for, `log` reads out the calls, a fork's answer may repeat what it was asked,
        // and none of those is the context holding the text
        let own = own.contains(&item.id) && !told_by_the_world(item);
        let mut hay = match own {
            true => String::new(),
            false => item.content.to_text().into_owned(),
        };
        if let Some(reasoning) = item.reasoning().filter(|_| !own) {
            hay.push('\n');
            hay.push_str(&reasoning.to_text());
        }
        for asked in item.calls().filter(|_| !own) {
            hay.push_str(&format!("\n{} {}", asked.tool, asked.args));
        }
        let lines: Vec<String> = hay
            .lines()
            .filter(|line| line.to_lowercase().contains(&needle))
            .map(around(&needle))
            .collect();
        if !lines.is_empty() {
            found.push((item, lines));
        }
    }

    let matches: usize = found.iter().map(|(_, lines)| lines.len()).sum();
    let where_ = match only.is_empty() {
        true => String::new(),
        false => format!(
            " (looking only in {})",
            only.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    // the ids that name nothing, said out loud rather than passed over. A search narrowed to an
    // item that is not there has not searched anything, and "nothing matched" is the one reading
    // of that which leaves the model believing the item exists
    let missing: Vec<String> = only
        .iter()
        .filter(|id| !items.iter().any(|item| item.id == **id))
        .map(|id| id.to_string())
        .collect();
    let unknown = match missing.as_slice() {
        [] => String::new(),
        [one] => format!(" There is no item {one}, so it was not among them."),
        _ => format!(
            " There are no items {}, so they were not among them.",
            missing.join(", ")
        ),
    };
    if matches == 0 {
        return format!(
            "no line of your context says `{text}`{where_}. Case was ignored, excluded items were \
             searched, and {read} item(s) were looked at.{unknown}\n",
        );
    }

    // rendered before it is priced, because the price is of this and not of an estimate of it
    let rendered: Vec<String> = found
        .iter()
        .flat_map(|(item, lines)| {
            lines
                .iter()
                .map(move |line| format!("  [{}] {}: {line}\n", item.id, item.label))
        })
        .collect();
    let counter = kernel.counter();
    let cost = counter.count(&nachalnik::Content::text(rendered.concat()));

    // and on an answer that did find something, for the same reason: an id that named nothing is
    // a fact about the call, and a result that only reports what it found lets it pass unnoticed
    let mut out = format!(
        "{} line(s) say `{text}`{where_}, ~{} tokens if you take them all, in {} item(s):{}\n",
        thousands(matches),
        thousands(cost),
        found.len(),
        match unknown.is_empty() {
            true => String::new(),
            false => format!("{unknown}\n"),
        },
    );
    for (item, lines) in &found {
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>4} line(s)  {}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            lines.len(),
            glimpse(&item.label),
        ));
    }

    let Some(take) = take else {
        out.push_str(
            "\n`take` shows that many of the lines. None of this puts an item into your request: \
             an excluded one is still excluded, and searching it changed nothing.\n",
        );

        return out;
    };

    // note: clamped, and said. A model that writes a "big enough" number - which is what a
    // search over a whole context invites - got the entire match set in one tool result, and the
    // compactor elided it on the way in, so the tool paid for the answer and the model was left
    // holding a marker. There is no way to page further, so the sentence says what to narrow
    // instead, rather than inviting a second call that will hand back the same first page
    let capped = take.min(TAKE);
    let shown = capped.min(rendered.len());
    match rendered.len() - shown {
        0 => out.push_str(&format!("\nall {} of them:\n", thousands(shown))),
        more => out.push_str(&format!(
            "\nthe first {}; {} more match and are not here:\n",
            thousands(shown),
            thousands(more)
        )),
    }
    if capped < take && shown < rendered.len() {
        out.push_str(&format!(
            "\n`take` is at most {}, so a wider one will not show the rest: a narrower `text`, or \
             `ids`, for the lines you did not get.\n",
            thousands(TAKE)
        ));
    }
    out.push_str(&rendered[..shown].concat());

    out
}

/// A matching line, trimmed to the part with the match in it.
///
/// note: around the match rather than from the start of the line, because the line a search finds
/// something in is as likely as not a minified one or a log line, and the first eighty characters
/// of those is the part nobody asked about.
fn around(needle: &str) -> impl Fn(&str) -> String + '_ {
    /// How much of a matching line comes back.
    const WINDOW: usize = 100;
    /// How much of it sits before the match, when there is room.
    const LEAD: usize = 30;

    move |line: &str| {
        let line = line.trim();
        if line.chars().count() <= WINDOW {
            return line.to_owned();
        }
        // note: counted in the lowercased line, which is where the offset came from. Lowercasing
        // can change a character's length - the Kelvin sign is three bytes and its `k` is one - so
        // the same offset into the line as written can land inside a character, which is a panic
        let lower = line.to_lowercase();
        let at = lower
            .find(needle)
            .map(|byte| lower[..byte].chars().count())
            .unwrap_or_default();
        let from = at.saturating_sub(LEAD);
        let said: String = line.chars().skip(from).take(WINDOW).collect();

        format!(
            "{}{said}{}",
            if from > 0 { "…" } else { "" },
            if from + WINDOW < line.chars().count() {
                "…"
            } else {
                ""
            },
        )
    }
}

/// What the next request costs, what there is, and what giving something up would buy.
///
/// note: the action a compaction decision is actually made from, which `look` was being asked to
/// be and is not: a table of every item in insertion order answers "what am I carrying?" and not
/// "what is it costing me and what should go?". The expensive items are the ones that decide that,
/// so they are sorted and totalled here rather than left to be found by reading.
///
/// note: it reports the estimate beside what the provider charged for the last request, and the
/// correction the counter has worked out from the difference, because the estimate is made without
/// the model's tokenizer and is usually low. An agent budgeting against a number nobody has
/// checked is the thing this crate exists not to do quietly.
///
/// note: the four ways of being held back are named and then divided in place, rather than counted
/// off by an ordinal. Every other number on this screen is an item id and the table under it opens
/// with a column of them, so "the first three" of four causes reads as *items 1, 2 and 3* - and a
/// model hunting for what those items are hiding spends calls, and carries what they fetch. An
/// ordinal in a tool whose output is a numbered table has two readings and costs whatever the
/// wrong one costs.
pub(super) fn budget(kernel: &Kernel, mine: &Mine) -> String {
    let budget = kernel.budget();
    let going = Going::of(kernel);
    let withheld: usize = kernel
        .items()
        .iter()
        .map(|item| going.held_back(item))
        .sum();

    let room = match budget.limit {
        Some(limit) => format!(
            " of {} ({}% full, ~{} left)",
            thousands(limit),
            (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
            thousands(limit.saturating_sub(budget.used())),
        ),
        None => ", against a limit this provider does not report".to_owned(),
    };

    let mut out = format!(
        "the next request is ~{} tokens{room}\n  {} in the context, {} in the tool definitions\n\
         ~{} tokens are being held back: excluded or elided to a marker, which you set - and the \
         program excludes too: a note you walked back, and the whole of a shortened answer; or \
         thinking this endpoint will not take back, which is not yours to change. `restore` puts \
         an excluded or elided item back\n",
        thousands(budget.used()),
        thousands(budget.context_tokens),
        thousands(budget.tool_tokens),
        thousands(withheld),
    );
    // straight after the figures it qualifies, as `/budget` puts it for the person: a picture reads
    // as nothing, so a context carrying one looks small when it is not
    if !budget.fully_counted() {
        out.push_str(&format!(
            "{} piece(s) of content nothing here can put a number on, so every figure above is a \
             floor and the real request is larger\n",
            budget.uncounted
        ));
    }

    match budget.reported {
        Some(usage) => out.push_str(&format!(
            "the last request really cost {} in / {}, as the provider counted it\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            crate::app::text::charged(&usage),
        )),
        None => out.push_str(
            "nothing has been charged for yet, so the figures above are only an estimate\n",
        ),
    }
    if let Some(learned) = kernel.counter().calibration() {
        out.push_str(&match learned.observations {
            0 => "the estimate has not been checked against a real request yet; treat it as a floor\n"
                .to_owned(),
            seen => format!(
                "the estimate is corrected by x{:.2}, learned from {seen} request(s)\n",
                learned.scale
            ),
        });
    }

    // the expensive ones, biggest first: what a decision about compaction is made from.
    //
    // note: what the *projection* included, not what the states say. They are not the same list -
    // a tool result whose call is not in the request is repaired out of it by the projector, and
    // is costing nothing however active it looks. Offering it as something to save tokens by
    // eliding would be advice that buys nothing
    //
    // note: and sorted by what each one *costs the request*, not by what it holds. Those are the
    // same figure for most rows and not for a turn that thought at length, which can hold tens of
    // thousands of tokens and put a thousand into the request. Ranked by what it held, it would
    // top a list headed "the most expensive items actually going into it", offering the agent
    // tens of thousands of tokens for an elision that frees a thousand. The column that decides is
    // the column to rank on.
    let mut costly: Vec<_> = kernel
        .items()
        .into_iter()
        .filter(|item| going.sends_content(item))
        .collect();
    costly.sort_by_key(|item| std::cmp::Reverse(going.costs.get(&item.id).copied().unwrap_or(0)));
    costly.truncate(10);

    if costly.is_empty() {
        return out;
    }

    out.push_str(&format!(
        "\nthe {} most expensive item(s) actually going into it:\n{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  what it is\n",
        costly.len(),
        "id",
        "state",
        "kind",
        "sending",
        "if all go",
    ));
    let mut running = 0;
    for item in &costly {
        let cost = going.costs.get(&item.id).copied().unwrap_or(0);
        running += cost;
        // saying so here saves a call that would only be refused, and the reason is the same one
        // a move would give: it is not the model's to move
        let whose = match protected(item, mine, None) {
            Some(_) => " · not yours",
            None => "",
        };
        // what it is holding on top of that, where it holds anything, because this row is where
        // an agent decides what to give up and the difference is the thing giving it up will not
        // free. A turn's thinking is gone from the request already
        let holding = match going.held_back(item) {
            0 => String::new(),
            held => format!(" · holding {} the request does not carry", thousands(held)),
        };
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {}{whose}{holding}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(cost),
            thousands(running),
            glimpse(&format!("{}: {}", item.label, item.content.to_text())),
        ));
    }
    out.push_str(
        "\nthe fifth column is what eliding everything down to that row would save, give or take \
         what the markers cost. A row that says it is holding something is holding it out of the \
         request already - giving that row up frees the fourth column and not the rest.\n",
    );

    out
}

/// The request that would go next, as a shape rather than as its bytes.
///
/// note: a summary and not the request itself, which is the one thing this could print and must
/// not: the request *is* the context, so answering with it would double every token the agent was
/// asking about. Roles, sizes and first lines are what the question "what am I about to send?"
/// actually wants, and `/request` in the terminal has the verbatim JSON for whoever wants that.
pub(super) fn request(kernel: &Kernel) -> String {
    let request = match kernel.preview_request() {
        Ok(request) => request,
        Err(e) => return format!("there is no request to preview: {e}\n"),
    };
    let projection = kernel.project();
    let budget = kernel.budget();

    let mut out = format!(
        "{} message(s), {} tool(s), ~{} tokens{}\n\n{:>4}  {:<10}  {:>8}  first line\n",
        request.messages.len(),
        request.tools.len(),
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(" of {}", thousands(limit)))
            .unwrap_or_default(),
        "#",
        "role",
        "bytes",
    );

    for (index, message) in request.messages.iter().enumerate() {
        let said = message
            .content
            .as_ref()
            .map(|c| c.to_text())
            .unwrap_or_default();
        // note: what goes out rather than what was said, which is what `to_text` answers: a turn
        // that only calls a tool says nothing and sends its arguments, and a picture's text is
        // its name. A turn carried as blocks holds its calls and its thinking in its content
        let bytes = message.content.as_ref().map_or(0, Content::byte_len)
            + message
                .tool_calls
                .iter()
                .map(ToolCall::byte_len)
                .sum::<usize>()
            + message.reasoning.as_ref().map_or(0, Content::byte_len);
        out.push_str(&format!(
            "{:>4}  {:<10}  {:>8}  {}\n",
            index + 1,
            message.role.as_str(),
            thousands(bytes),
            glimpse(&said),
        ));
    }

    // note: split by *what* dropped it, because the two halves are answered differently and one
    // list could not say which was which. An item left out by its own state is one `restore`
    // puts straight back. An item the projector dropped is a consequence of something else in
    // the context, and restoring it does nothing whatever - the thing to move is the cause. A
    // model reading one undifferentiated list has to guess which it is looking at, and the cheap
    // guess is `restore`, which is the one that changes nothing and costs a call.
    let (by_state, by_projector): (Vec<_>, Vec<_>) =
        projection
            .skipped
            .iter()
            .partition(|left_out| match kernel.item(left_out.id) {
                Some(item) => !item.is_projected(),
                // an id the context no longer has is nothing a state change can reach either
                None => false,
            });

    if !by_state.is_empty() {
        out.push_str("\nleft out by its own state, which `restore` puts back:\n");
        for left_out in &by_state {
            out.push_str(&format!("  [{}] {}\n", left_out.id, left_out.reason));
        }
    }
    if !by_projector.is_empty() {
        out.push_str(&format!(
            "\nleft out by the projector (`{}`), the rule that turns your context into messages. \
             Restoring these changes nothing - each is a consequence of something else, and the \
             cause is what there is to move:\n",
            kernel.projector().name(),
        ));
        for left_out in &by_projector {
            out.push_str(&format!("  [{}] {}\n", left_out.id, left_out.reason));
        }
    }
    if !projection.repairs.is_empty() {
        out.push_str("\nand what that same projector rewrote, to keep the request valid:\n");
        for repair in &projection.repairs {
            out.push_str(&format!("  {repair}\n"));
        }
    }
    // note: under a heading of its own, and one that says nothing was lost. This tool is read by a
    // model deciding what to do next, and "rewrote, to keep the request valid" over a line about a
    // tool result changing places is an invitation to go and fix something that is not broken -
    // which is what a `note` produces, every single time it is written
    if !projection.reordered.is_empty() {
        out.push_str(
            "\nand what it put in a different order than your context holds it, which costs \
             nothing and is nothing to act on:\n",
        );
        for moved in &projection.reordered {
            out.push_str(&format!("  {moved}\n"));
        }
    }

    out
}

/// The first line of something, shortened to fit a column.
fn glimpse(text: &str) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    match first.chars().count() > GLIMPSE {
        true => format!("{}…", first.chars().take(GLIMPSE - 1).collect::<String>()),
        false => first.to_owned(),
    }
}
