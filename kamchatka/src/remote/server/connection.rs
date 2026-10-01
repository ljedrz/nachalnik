//! One client's conversation with a served session, start to finish: the handshake, the records
//! it is caught up on, the questions it is asked, and the commands it sends - none of which holds
//! the session itself.

use std::sync::Arc;

use nachalnik::{Event, Kernel};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::{Notify, broadcast, mpsc, oneshot},
};

use crate::{
    app::text,
    remote::protocol::{self, Command, Message},
};

use super::{Answered, FromClient, name};

/// One connection, from the moment it arrives to the moment it goes.
pub(super) async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    client: u64,
    stream: S,
    kernel: Kernel,
    asks: mpsc::UnboundedSender<FromClient>,
    replaced: Arc<Notify>,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut frames = protocol::Frames::new(BufReader::new(read));
    if let Err(e) = attend(client, &mut frames, &mut write, &kernel, &asks, &replaced).await {
        // the connection is going either way; this is the last thing it is told, and it is written
        // on a best-effort basis because the usual way to be here is that it stopped listening
        //
        // note: and there is a second way, which is the one the cap exists for. A peer that is
        // still sending when this closes leaves data in the receive buffer nobody read, and TCP
        // answers a close like that with a reset - which on some platforms discards what the peer
        // had already been sent, this sentence among it. Draining first would deliver it and is
        // exactly what `MAX_LINE` refuses to do: not reading a peer that floods is the point, so
        // the sentence is the thing that gives. What a client can rely on is that the connection
        // ends, not that it is told why
        let _ = protocol::write(
            &mut write,
            &Message::Failed {
                about: "the connection".to_owned(),
                error: e,
            },
        )
        .await;
    }
    let _ = asks.send(FromClient::Left { client });
}

/// The connection proper, up to whatever ends it.
///
/// note: the two streams a client reads are both taken from a [`Kernel`] handle of this
/// connection's own, and neither is handed to it by the session loop. The numbered one is the log,
/// read with `history_since` from wherever this client says it had got to, which is why a client
/// that falls behind, lags its subscription or loses its socket cannot lose a record: the log is
/// not a queue and nothing drains it. The unnumbered one is the subscription, which carries the
/// fragments - and doubles as the thing that says it is worth looking at the log again.
async fn attend<R, W>(
    client: u64,
    frames: &mut protocol::Frames<BufReader<R>>,
    write: &mut W,
    kernel: &Kernel,
    asks: &mpsc::UnboundedSender<FromClient>,
    replaced: &Notify,
) -> Result<(), String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // note: subscribed *before* the first look at the log rather than after, so that an event
    // landing between the two is a duplicate wakeup rather than a record nobody went back for
    let mut events = kernel.subscribe();
    // note: whether the fragments are in the log as well. With `record_progress` on they are
    // records, so sending them from here too would put every fragment on the wire twice - once
    // numbered and once not - and a client assembling an answer out of both would read it double
    let progress_recorded = kernel.config().record_progress;

    // note: a connection says where it stands before it is told anything, and nothing else is
    // accepted first. Streaming at a client that has not said what it already has is how a resume
    // becomes a replay
    let settled = match protocol::read::<Command>(frames).await? {
        None => return Ok(()),
        Some(attach @ Command::Attach { .. }) => {
            watermark(attach, kernel, asks, client, write).await
        }
        Some(other) => {
            return Err(format!(
                "`{}` before `attach`: a connection says where it stands first",
                name(&other)
            ));
        }
    };
    // note: reported as the attach's failure rather than the connection's, because a client that
    // can tell a refused watermark from a broken socket has something to do about it - come back
    // with none. Told only that the connection failed, it would read the close as a drop and retry
    // the same impossible resume until it gave up
    let (mut last, mut voice) = match settled {
        Ok(settled) => settled,
        Err(refused) => return refuse(write, refused.about, refused.error).await,
    };
    flush(kernel, &mut last, write).await?;

    loop {
        tokio::select! {
            // note: caught up first, so that a client replaced mid-turn has the records up to the
            // moment it was let go of - what it does with them is its own business - and then
            // named, so that it can tell being replaced from a drop. A client that read the close
            // as a drop would come back and take the session from whoever has just taken it
            () = replaced.notified() => {
                flush(kernel, &mut last, write).await?;

                return refuse(
                    write,
                    "replaced",
                    "another client has attached to this session, and it serves one at a time; \
                     attaching again takes it back"
                        .to_owned(),
                )
                .await;
            }
            command = protocol::read::<Command>(frames) => match command? {
                None => return Ok(()),
                // note: re-attaching on a live connection is allowed, and is the cheapest way for a
                // client that has confused itself to start again: it asks for the projection and
                // moves its own watermark to whatever that says
                Some(attach @ Command::Attach { .. }) => {
                    let since = matches!(attach, Command::Attach { since: None, .. });
                    let settled = watermark(attach, kernel, asks, client, write).await;
                    let (at, fresh) = match settled {
                        Ok(settled) => settled,
                        // note: caught up first, because the usual way to be refused on a live
                        // connection is the session ending under the attach - and this is the
                        // last chance to send `session.finished` to a client that is still here
                        Err(refused) => {
                            flush(kernel, &mut last, write).await?;

                            return refuse(write, refused.about, refused.error).await;
                        }
                    };
                    last = at;
                    // note: the subscription is swapped exactly where a projection is handed over,
                    // because that is what they have to be taken together for. A resume on a live
                    // connection is answered with no projection, so the one it already has is
                    // holding lines nothing else would bring back
                    if since {
                        voice = fresh;
                    }
                    flush(kernel, &mut last, write).await?;
                }
                // answered here rather than by the session loop: it needs a `Kernel` and nothing
                // else, and the session has better things to be doing
                //
                // note: what an item says *now*, which is the kernel's. An earlier version is not
                // - the viewer keeps those, and the viewer is the `App` - so a question about one
                // falls through to the branch below and is answered where they are. Reading them
                // here would mean the session holding what it has already given away
                Some(Command::Inspect {
                    id,
                    raw,
                    version: None,
                }) => {
                    let message = match kernel.items().iter().find(|item| item.id == id) {
                        // the item's own text where a client is about to put it in front of
                        // somebody to edit, and the reading of it where somebody is going to read
                        // it. `Kernel::replace` writes content, so the reading is the one thing
                        // that must never come back as an edit
                        Some(item) => Message::Item {
                            id,
                            body: match raw {
                                true => item.content.to_text().into_owned(),
                                false => text::stored(item),
                            },
                            raw,
                            version: None,
                        },
                        None => Message::Failed {
                            about: "inspect".to_owned(),
                            error: format!("there is no item {id} in the context"),
                        },
                    };
                    answer(write, &message, "inspect").await?;
                }
                Some(command) => {
                    let about = name(&command);
                    match ask(asks, client, command).await.map(|it| it.message) {
                        // note: caught up first, for the reason `Message::Busy` is: a reply carries
                        // `busy`, and a client whose input has closed leaves on `busy: false` - so
                        // `/note keep this` piped in and answered ahead of its `context.added` was
                        // a client gone before the record of what it had just done
                        Ok(Some(message)) => {
                            caught_up(&mut events, kernel, &mut last, write, progress_recorded)
                                .await?;
                            answer(write, &message, about).await?
                        }
                        Ok(None) => {}
                        // note: the session has ended with this command still waiting on it, and
                        // the connection ends here rather than in the branch below that would have
                        // written the last of the log - so it is written here. Without it the
                        // client that typed `/quit` and then anything else is never sent
                        // `session.finished`, reads the close as a drop, and goes looking for a
                        // session that is gone
                        Err(error) => {
                            flush(kernel, &mut last, write).await?;
                            protocol::write(write, &Message::Failed {
                                about: about.to_owned(),
                                error,
                            })
                            .await?;

                            return Ok(());
                        }
                    }
                }
            },
            event = events.recv() => match event {
                Ok(event) => {
                    // the numbered half first, always. A fragment names the last record the client
                    // was sent, so one written before that record had gone out would be naming a
                    // number nobody had seen
                    //
                    // note: the cost is that a connection behind the broadcast can be handed a
                    // fragment *after* the record that ends the thing it was part of, because the
                    // flush reads wherever the log has got to rather than wherever it had got to
                    // when the fragment was emitted. Reconstructing the true interleaving would
                    // mean flushing one record per non-progress event - exact, and a linear scan
                    // of the log per event, per client. It is not worth it: a fragment whose item
                    // has already arrived is a fragment the item supersedes. The terminal drops
                    // one on those grounds, under the name `Entry::transient`; `--connect` prints
                    // the answer from fragments alone, so a late one is printed late there
                    flush(kernel, &mut last, write).await?;
                    if protocol::is_progress(&event) && !progress_recorded {
                        protocol::write(write, &Message::Progress { after: last, event }).await?;
                    }
                }
                // note: the numbered records are not what this is about - `flush` reads them out
                // of the log, which nothing drains, so no record is ever lost here. What went
                // past is the unnumbered half, and none of it is in the log to begin with: a
                // model's fragments are dropped by the runtime and this session's own lines are
                // said rather than recorded. So the client is told what it missed and not sent
                // after the records for it
                Err(broadcast::error::RecvError::Lagged(frames)) => {
                    flush(kernel, &mut last, write).await?;
                    protocol::write(write, &Message::Missed { frames }).await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            said = voice.recv() => match said {
                // note: the numbered half first, for the reason the event branch above gives and
                // for a sharper one. `Message::Busy` is how a client learns a turn is over, and a
                // client whose input has closed takes that at its word and leaves - so a
                // `busy: false` written *before* the records of the turn it is about says the turn
                // is done while the last of it is still in the log, and `kamchatka --connect` with
                // a question piped into it detaches without the answer
                Ok(message) => {
                    caught_up(&mut events, kernel, &mut last, write, progress_recorded).await?;
                    protocol::write(write, &*message).await?;
                }
                // the program's own lines are in no log either, and the same rule applies to them
                Err(broadcast::error::RecvError::Lagged(frames)) => {
                    protocol::write(write, &Message::Missed { frames }).await?;
                }
                // note: the session has ended, and this is the last thing this connection does:
                // write out whatever the log grew while it was being written to. Which branch sees
                // the end first is a coin toss - `session.finished` is emitted before this channel
                // is dropped, so both are ready at once and `select!` picks either - and what the
                // toss costs is a client left short of the last records of a session it has no
                // socket left to go back for them on.
                //
                // note: no test fails without this, because in practice the event branch is ready
                // first. The race is real and rare, and the flush is kept on those terms
                Err(broadcast::error::RecvError::Closed) => {
                    flush(kernel, &mut last, write).await?;

                    return Ok(());
                }
            },
        }
    }
}

/// A refusal, and which of the client's commands to name it as.
///
/// note: three names for what one function refuses, because the three are not the same news. An
/// `attach` refusal is mended by attaching afresh, which is what a client does with it; a `version`
/// refusal is not mended by anything, and a client that treats it the same way reattaches, is
/// refused identically, and gives up a minute later saying the session has not answered, when it
/// answered at once. A `projection` refusal is a third kind: the session is answering perfectly
/// well and it is this attach that cannot be served, because the projection is larger than a
/// client reads - so attaching again gets the identical answer, and the only thing a client can do
/// is read the records without one. See [`crate::remote::Client`].
struct Refused {
    /// The command to name it as: `attach`, `version`, or `projection`.
    about: &'static str,
    /// What went wrong.
    error: String,
}

impl From<String> for Refused {
    /// Anything else that stops an attach is the attach's.
    fn from(error: String) -> Self {
        Self {
            about: "attach",
            error,
        }
    }
}

/// Settles where this client's numbered stream starts, and sends the projection if it needs one.
///
/// note: the subscription to the program's own voice comes back with the watermark, because the
/// session loop is where both are taken and it takes them together. See [`Answered`].
async fn watermark<W: AsyncWrite + Unpin>(
    attach: Command,
    kernel: &Kernel,
    asks: &mpsc::UnboundedSender<FromClient>,
    client: u64,
    write: &mut W,
) -> Result<(u64, broadcast::Receiver<Arc<Message>>), Refused> {
    let Command::Attach {
        since,
        session,
        version,
    } = attach
    else {
        return Err("that is not an attach".to_owned().into());
    };
    // note: a version this session does not know is refused before anything else is read off the
    // message, because what the rest of it means is the thing in question. An older one it does
    // know is served - see `protocol::VERSION`
    let spoken = version.unwrap_or(1);
    if spoken > protocol::VERSION {
        return Err(Refused {
            about: "version",
            error: format!(
                "you speak version {spoken} of this protocol and this session speaks {}; the \
                 older end is this one",
                protocol::VERSION
            ),
        });
    }
    let Some(since) = since else {
        let answered = ask(
            asks,
            client,
            Command::Attach {
                since: None,
                session: None,
                version: None,
            },
        )
        .await?;
        let (Some(Message::Attached(attached)), Some(voice)) = (answered.message, answered.voice)
        else {
            return Err("the session answered an attach with something else"
                .to_owned()
                .into());
        };
        let seq = attached.seq;
        // note: the size check `flush` and `answer` both apply, and it is here because a projection
        // is a frame like any other and this was the one write that did not ask. A message larger
        // than `MAX_LINE` in the context makes the projection itself that long - unlike a record,
        // which can be named and moved past, a projection cannot be skipped, so a client that
        // cannot read it is a client with no session at all. Sent unchecked it is a frame the
        // other end refuses, which closes the connection and reads as a drop, and the client
        // spends a minute reattaching to a session that answered every time.
        //
        // note: refused by name rather than written and hoped for. What a client can do about this
        // is read the records without a projection, so that is what the sentence says; abridging
        // the projection is a decision about what every client is handed and is not taken here.
        // See `POSTPONED.md`.
        let projection = Message::Attached(attached);
        let line = protocol::framed(&projection)?;
        if let Some(bytes) = protocol::overlong(&line) {
            return Err(Refused {
                about: "projection",
                error: format!(
                    "this session cannot be attached: its projection is {bytes} bytes, more than \
                     a client reads in one line ({}). The records are still there and are read \
                     without a projection - `inspect ID` fetches any one item - but the whole \
                     conversation at once does not fit in one line",
                    protocol::MAX_LINE
                ),
            });
        }
        protocol::write_frame(write, &line).await?;

        return Ok((seq, voice));
    };

    // note: the loud half of the same check, and it is first because it is the one that catches
    // the quiet case. A session restarted at this address has a log of its own, and a watermark
    // from the one before it can be perfectly plausible against it - at which point the client
    // draws one session's records under another's conversation with nothing anywhere saying so.
    // A client that does not name a session is not made to; see `Command::Attach`
    let named = kernel.session_name();
    if session.is_some_and(|session| session != named) {
        return Err(format!(
            "you are resuming a session this is not: this one is `{named}`, and attaching with no \
             `since` starts again here"
        )
        .into());
    }
    // note: a client claiming to have seen more than has happened is refused rather than clamped.
    // It is either a client that has confused two sessions or one that made the number up, and
    // quietly starting it from the end would leave it convinced it held a history it never had
    let last = kernel.last_seq();
    if since > last {
        return Err(format!(
            "this session has {last} record(s) and you say you have {since}; attach with no \
             `since` to start again"
        )
        .into());
    }
    // note: refused above without troubling the session, and answered here by the session itself,
    // because the answer carries `busy` and nothing but the loop driving the kernel knows it
    let mut last = since;
    let answered = match ask(
        asks,
        client,
        Command::Attach {
            since: Some(since),
            session: None,
            version: None,
        },
    )
    .await
    {
        Ok(answered) => answered,
        // the session ended while this was on its way in: what it missed is still owed, and the
        // end of the log is the one thing that tells it not to come back
        Err(error) => {
            flush(kernel, &mut last, write).await?;

            return Err(error.into());
        }
    };
    let (Some(message), Some(voice)) = (answered.message, answered.voice) else {
        return Err("the session answered an attach with nothing"
            .to_owned()
            .into());
    };
    // note: what was missed before the answer, because the answer carries `busy` and a client
    // whose input has closed leaves on `busy: false` - the rule `Message::Busy` and a command's
    // reply are held to. Written after, a client coming back to collect the end of an answer was
    // told the session was resting and left before the records it had come back for
    flush(kernel, &mut last, write).await?;
    for standing in &answered.standing {
        protocol::write(write, standing).await?;
    }
    protocol::write(write, &message).await?;

    Ok((last, voice))
}

/// Says a command could not be done, and ends the connection on it.
///
/// note: named rather than reported as the connection's, so that a failure a client can do
/// something about reads as itself - and named one of two ways, because what there is to do about
/// the two differs. See [`Refused`] and the first attach in [`attend`].
async fn refuse<W: AsyncWrite + Unpin>(
    write: &mut W,
    about: &str,
    error: String,
) -> Result<(), String> {
    protocol::write(
        write,
        &Message::Failed {
            about: about.to_owned(),
            error,
        },
    )
    .await
}

/// Puts one command to the session loop and waits for what it says.
async fn ask(
    asks: &mpsc::UnboundedSender<FromClient>,
    client: u64,
    command: Command,
) -> Result<Answered, String> {
    let (answer, answered) = oneshot::channel();
    asks.send(FromClient::Asked {
        client,
        command,
        answer,
    })
    .map_err(|_| "the session has ended".to_owned())?;

    answered
        .await
        .map_err(|_| "the session did not answer".to_owned())
}

/// Writes out everything the session has already emitted, numbered and not.
///
/// note: what this is for is `Message::Busy`. A client with its input closed leaves when the
/// session goes quiet, and `busy: false` is how it finds out - so that message overtaking the
/// fragments of the turn it is about is a client detaching without the answer. The records cannot
/// stand in for them: the log names what happened and does not copy it, so `context.added` says an
/// assistant turn exists and not one word of what it said. What the model actually *said* reaches a
/// client only as `Message::Progress`, and only if it is written first.
///
/// note: neither end is wrong without this. The session records the answer and stops the turn, and
/// the client leaves when it is told the session has nothing left to do; the fault is only in the
/// order they are written in.
async fn caught_up<W: AsyncWrite + Unpin>(
    events: &mut broadcast::Receiver<Event>,
    kernel: &Kernel,
    last: &mut u64,
    write: &mut W,
    progress_recorded: bool,
) -> Result<(), String> {
    loop {
        match events.try_recv() {
            Ok(event) => {
                flush(kernel, last, write).await?;
                if protocol::is_progress(&event) && !progress_recorded {
                    let after = *last;
                    protocol::write(write, &Message::Progress { after, event }).await?;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(frames)) => {
                flush(kernel, last, write).await?;
                protocol::write(write, &Message::Missed { frames }).await?;
            }
            // nothing waiting, or the session has ended and the log is the last word either way
            Err(_) => return flush(kernel, last, write).await,
        }
    }
}

/// Writes the answer to a command, or says why it cannot be sent.
///
/// note: the rule [`flush`] holds records to, for the answers that carry content. An `inspect` of
/// an item past `MAX_LINE` is a frame the client refuses, and refusing one closes the connection -
/// and `inspect` is how the protocol tells a client to get past an `Oversized` record, so without
/// this the way out would be a way off the session.
async fn answer<W: AsyncWrite + Unpin>(
    write: &mut W,
    message: &Message,
    about: &str,
) -> Result<(), String> {
    let line = protocol::framed(message)?;
    match protocol::overlong(&line) {
        Some(bytes) => {
            protocol::write(
                write,
                &Message::Failed {
                    about: about.to_owned(),
                    error: format!(
                        "the answer is {bytes} bytes, more than a client reads in one line ({})",
                        protocol::MAX_LINE
                    ),
                },
            )
            .await
        }
        None => protocol::write_frame(write, &line).await,
    }
}

/// Writes out every record the session has grown since this client last saw one.
///
/// note: the log rather than the subscription, which is why a client cannot lose one. A broadcast
/// has a capacity and drops for whoever falls behind it; the log has neither, so this is a read
/// from wherever this connection had got to and it is correct however long the connection was
/// asleep, however far behind its subscription fell, and whether or not it was here at all when
/// the record was written.
async fn flush<W: AsyncWrite + Unpin>(
    kernel: &Kernel,
    last: &mut u64,
    write: &mut W,
) -> Result<(), String> {
    // note: asked first because it is a read of one number, where `history_since` searches the log
    // and copies what follows it under the lock every emit waits on. This runs for every event,
    // `model.delta` included, for every connection - and most of those events add no record this
    // connection has not already been sent
    if kernel.last_seq() <= *last {
        return Ok(());
    }
    for record in kernel.history_since(*last) {
        let seq = record.seq;
        let line = protocol::framed(&Message::Record(record))?;
        // note: a record the other end would refuse to read is named rather than sent, and the
        // naming carries its sequence - so the client takes it as seen and resumes after it. Sent,
        // it is a frame over `MAX_LINE`, which closes the connection; and because a client resumes
        // by sequence it would come straight back to the same record on every attempt, and one
        // `context.replaced` over the limit would lock everybody out for the rest of the session.
        // See `Message::Oversized`
        match protocol::overlong(&line) {
            Some(bytes) => protocol::write(write, &Message::Oversized { seq, bytes }).await?,
            None => protocol::write_frame(write, &line).await?,
        }
        *last = seq;
    }

    Ok(())
}
