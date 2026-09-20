//! A client of somebody else's session, in about a hundred lines and over a port.
//!
//! ```console
//! $ kamchatka --serve tcp:127.0.0.1:7878 -m mercury-2
//! · serving on tcp:127.0.0.1:7878
//! ```
//!
//! ```console
//! $ cargo run --example attached -- tcp:127.0.0.1:7878 "what is 2+2"
//! --- 2026-09-12T16-25-31Z, 9 records, 0 items, ~526 tokens, mercury-2 ---
//! · serving on tcp:127.0.0.1:7878: the session is this program's rather than any client's …
//! · client 1 attached
//! ask: what is 2+2
//! --- the session took it: Asked(ContextId(1)) ---
//! 4
//! --- the turn ended; 7 records arrived, and item 2 is where those fragments ended up ---
//! 4
//! ```
//!
//! note: this reaches for [`kamchatka::remote::protocol`], `tokio`, and three plain data types
//! that `protocol` itself names - `Speaker`, `Did` and `Page`, which are an enum of seven, an enum
//! of three, and two strings. It touches no `App`, no wiring and not `remote::Client`, and that is
//! the point of it rather than something it happens to do: `remote/` says that when something
//! which is not `kamchatka` needs to speak this, `protocol` is what moves, and the honest version
//! of that sentence is `protocol` **and those three** - which is what an example rather than a
//! paragraph is for. They are the program's own vocabulary rather than the wire's second copy of
//! it, which is why they are borrowed instead of redeclared.
//!
//! note: a client in another language needs none of that and no Rust at all. The wire is
//! newline-delimited JSON, one object per line:
//!
//! ```text
//! → {"do":"attach","since":null,"session":null,"version":1}
//! ← {"is":"attached","version":1,"session":"…","seq":9,"busy":false,"conversation":[…], …}
//! → {"do":"submit","line":"what is 2+2"}
//! ← {"is":"replied","did":{"asked":1},"page":null,"busy":true}
//! ← {"is":"record","seq":10,"at":1789226532741,"event":{"event":"context.added","id":1, …}}
//! ← {"is":"progress","after":12,"event":{"event":"model.delta","delta":{"text":"4"}}}
//! ← {"is":"busy","busy":false}
//! ```
//!
//! note: it refuses every permission question it is asked, and says so. That is what an unattended
//! client should do - it is the same default `--on-ask` has - and a real one puts the question in
//! front of a person instead. The shape of the answer is the interesting part either way.

use std::time::Duration;

use kamchatka::remote::protocol::{self, Address, Command, Message};
use nachalnik::{Delta, Event, Grant};
use tokio::{io::BufReader, net::TcpStream};

/// How long to wait for a session that has said nothing at all.
const PATIENCE: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let address = args.next().ok_or(
        "usage: attached tcp:HOST:PORT [MESSAGE]...\n\
         start a session with `kamchatka --serve tcp:127.0.0.1:7878` first",
    )?;
    let question = args.collect::<Vec<_>>().join(" ");

    let Address::Tcp(host) = protocol::address(&address)? else {
        return Err("this example speaks to a port; `--serve tcp:127.0.0.1:PORT`".to_owned());
    };
    let stream = TcpStream::connect(host)
        .await
        .map_err(|e| format!("could not reach {host}: {e}"))?;
    // the two options a port wants and a socket file does not: send small frames when they are
    // written rather than when the last one is acknowledged, and find out when the peer is gone
    let _ = stream.set_nodelay(true);
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = protocol::Frames::new(BufReader::new(read));

    // a connection says where it stands before it is told anything. `since: None` is "I have
    // nothing", which is answered with the projection and then every record after it
    protocol::write(
        &mut write,
        &Command::Attach {
            since: None,
            // nothing to name and nothing to resume: this client attaches once and leaves
            session: None,
            version: Some(protocol::VERSION),
        },
    )
    .await?;
    let Some(Message::Attached(attached)) = next(&mut lines).await? else {
        return Err("the session did not answer an attach with a projection".to_owned());
    };
    println!(
        "--- {}, {} records, {} items, ~{} tokens{} ---",
        attached.session,
        attached.seq,
        attached.items.len(),
        attached.budget.used(),
        match &attached.model {
            Some(model) => format!(", {}", model.model),
            None => String::new(),
        }
    );
    // note: the conversation comes from *here* and can come from nowhere else. The records name
    // things rather than carrying them - `context.added` says there is a user message of eleven
    // tokens and not one word of what it says - so a client fed nothing but the stream can follow a
    // turn as it arrives and cannot render a syllable of what happened before it connected
    for line in &attached.conversation {
        println!("{} {}", mark(line.speaker), line.text);
    }
    if question.is_empty() {
        println!("--- nothing to ask, so nothing was asked ---");

        return Ok(());
    }

    println!("ask: {question}");
    protocol::write(&mut write, &Command::Submit { line: question }).await?;

    // note: what says the turn is over is `busy`, and it has to be: a turn is a loop over
    // transitions, so `state.changed` reaches `Idle` between two requests of one turn. Every
    // command is answered exactly once and the answer carries `busy`, which is how this knows a
    // turn started at all
    let mut busy = true;
    let mut records = 0;
    let mut answer = None;
    while busy {
        let Some(message) = next(&mut lines).await? else {
            return Err("the session went away mid-turn".to_owned());
        };
        match message {
            Message::Replied { did, busy: now, .. } => {
                println!("--- the session took it: {did:?} ---");
                busy = now;
            }
            Message::Busy { busy: now } => busy = now,
            // a fragment of a model still writing: unnumbered, best-effort, and gone once past
            Message::Progress {
                event:
                    Event::ModelDelta {
                        delta: Delta::Text(text),
                    },
                ..
            } => {
                // flushed, because this is the one thing the example is for: a model's sentence
                // arriving as it is written. stdout is line-buffered, so without this the
                // streaming shows up a line at a time, which is what not streaming looks like
                print!("{text}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            Message::Record(record) => {
                records += 1;
                match &record.event {
                    Event::ToolRequested { tool, args, .. } => println!("\n⟩ {tool}({args})"),
                    Event::ModelFinished { item, .. } => answer = Some(*item),
                    // refused, and said out loud. An unattended client that allowed things would be
                    // deciding on somebody's behalf the one thing this whole program exists not to
                    // decide on their behalf
                    Event::PermissionRequested { request } => {
                        println!("\n? {} wanted {}; refusing", request.tool, request.args);
                        protocol::write(
                            &mut write,
                            &Command::Decide {
                                id: request.id,
                                grant: Grant::Deny,
                                remember: false,
                            },
                        )
                        .await?;
                    }
                    _ => {}
                }
            }
            Message::Said { text, .. } => println!("\n· {text}"),
            Message::Failed { about, error } => println!("\n· {about}: {error}"),
            _ => {}
        }
    }

    // and the whole of what any one item holds is one command away, which is the other half of the
    // records naming things rather than carrying them
    if let Some(item) = answer {
        protocol::write(&mut write, &Command::Inspect { id: item }).await?;
        if let Some(Message::Item { body, .. }) = next(&mut lines).await? {
            // note: the same words twice, on purpose. Above they were fragments of a model still
            // writing, which are in no log and are gone once they have gone past; this is the item
            // they were recorded as, which is what a client that was not here for them would ask
            // for. Seeing both is the clearest way to see that they are two different things
            println!(
                "\n--- the turn ended; {records} records arrived, and item {item} is where those \
                 fragments ended up ---"
            );
            println!("{body}");
        }
    }

    // note: nothing ends the session here. Dropping the connection detaches; the session is the
    // serving program's and carries on without this one, which is the invariant the whole protocol
    // is arranged around. `/quit` as a `submit` is how a client says it meant to end one
    Ok(())
}

/// The one character a line of the conversation is printed under.
///
/// note: a client decides this for itself, which is the reason `Speaker` is on the wire at all.
/// What the session hands over is who was talking; how that looks is nobody's business but the
/// screen's, and this one has seven characters where a browser would have seven colours.
fn mark(speaker: kamchatka::app::Speaker) -> &'static str {
    use kamchatka::app::Speaker::*;

    match speaker {
        User => ">",
        Model => " ",
        Reasoning => "~",
        Call => "⟩",
        Result => "<",
        Note => "·",
        Error => "!",
    }
}

/// The next message, or a failure saying none came.
async fn next<R: tokio::io::AsyncRead + Unpin>(
    lines: &mut protocol::Frames<BufReader<R>>,
) -> Result<Option<Message>, String> {
    tokio::time::timeout(PATIENCE, protocol::read(lines))
        .await
        .map_err(|_| format!("the session said nothing for {}s", PATIENCE.as_secs()))?
}
