//! One client's conversation with a served session: the handshake, the records it catches up on,
//! the questions it is asked, and the commands it sends. None of these holds the session itself.

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
        // the connection is closing either way. This is the last message the client receives, and
        // the write is best-effort because the usual reason for reaching this code is that the
        // client stopped listening.
        //
        // note: there is a second way to get here, the one `MAX_LINE` exists for. A peer that is
        // still sending when the connection closes leaves unread data in the receive buffer, and
        // TCP answers such a close with a reset. On some platforms the reset discards data already
        // sent to the peer, including this message. Draining the buffer first would deliver the
        // message, but `MAX_LINE` refuses to do that: not reading a flooding peer is the point, so
        // this message is what is lost. A client can rely on the connection ending, not on being
        // told why
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

/// Handles the connection until something ends it.
///
/// note: both streams a client reads come from a [`Kernel`] handle owned by this connection, not
/// from the session loop. The numbered stream is the log, read with `history_since` from the
/// position this client reports. A client that falls behind, lags its subscription, or loses its
/// socket cannot lose a record, because the log is not a queue and nothing drains it. The
/// unnumbered stream is the subscription. It carries fragments and signals that the log is worth
/// reading again.
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
    // note: subscribe before the first log read, not after, so an event that arrives between
    // subscribing and reading is a duplicate wakeup rather than a missed record
    let mut events = kernel.subscribe();
    // note: with `record_progress` on, fragments are already records in the log. Sending them here
    // too puts every fragment on the wire twice - once numbered and once not - and a client
    // assembling an answer from both reads it double
    let progress_recorded = kernel.config().record_progress;

    // note: a connection says where it stands before anything else is accepted. Streaming at a
    // client that has not said what it already has turns a resume into a replay
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
    // note: report a refused watermark as the attach's failure, not the connection's. A client that
    // can tell a refused watermark from a broken socket can come back with no watermark. If it were
    // told only that the connection failed, it would read the close as a drop and retry the same
    // impossible resume until it gave up
    let (mut last, mut voice) = match settled {
        Ok(settled) => settled,
        Err(refused) => return refuse(write, refused.about, refused.error).await,
    };
    flush(kernel, &mut last, write).await?;

    loop {
        tokio::select! {
            // note: caught up first, so a client replaced mid-turn has the records up to the moment
            // it was let go, then named so it can tell replacement from a drop. A client that reads
            // the close as a drop would come back and take the session from whoever has just taken
            // it
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
                // note: re-attaching on a live connection is allowed. It is the cheapest way for a
                // confused client to start again: it asks for the projection and moves its
                // watermark to whatever that says
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
                    // note: the subscription is swapped exactly where the projection is handed
                    // over, because the two must be taken together. A resume on a live connection
                    // is answered with no projection, so the existing subscription is still holding
                    // lines that nothing else would bring back
                    if since {
                        voice = fresh;
                    }
                    flush(kernel, &mut last, write).await?;
                }
                // answered here rather than by the session loop because it needs a `Kernel` and
                // nothing else.
                //
                // note: what an item says now is the kernel's. The viewer keeps earlier versions,
                // and the viewer is the `App`, so a question about one falls through to the branch
                // below and is answered there. Reading them here would mean the session holding
                // what it has already given away
                Some(Command::Inspect {
                    id,
                    raw,
                    version: None,
                }) => {
                    let message = match kernel.items().iter().find(|item| item.id == id) {
                        // the item's own text for a client about to edit it, and the reading for a
                        // reader. `Kernel::replace` writes content, so the reading must never come
                        // back as an edit
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
                    match ask(asks, client, command).await {
                        // note: caught up first, for the reason `Message::Busy` is: a reply carries
                        // `busy`, and a client whose input has closed leaves on `busy: false` - so
                        // `/note keep this` piped in and answered ahead of its `context.added` was
                        // a client gone before the record of what it had just done
                        //
                        // note: and what is for this client alone - the text of a `/copy` - just
                        // before the answer, so it has arrived by the time the line is answered
                        Ok(Answered {
                            message: Some(message),
                            standing,
                            ..
                        }) => {
                            caught_up(&mut events, kernel, &mut last, write, progress_recorded)
                                .await?;
                            for message in &standing {
                                voiced(write, message).await?;
                            }
                            answer(write, &message, about).await?
                        }
                        Ok(Answered { message: None, .. }) => {}
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
                    // Write the numbered half first, always. A fragment names the last record the
                    // client was sent; written before that record went out, it names a number
                    // nobody had seen.
                    //
                    // note: a connection behind the broadcast can receive a fragment after the
                    // record that ends its item, because flush reads the log as it is now, not as
                    // it was when the fragment was emitted. Exact interleaving would mean flushing
                    // one record per non-progress event per client - a linear scan of the log per
                    // event - so it is not worth it: a fragment whose item has already arrived is
                    // superseded by that item. The terminal drops it as `Entry::transient`;
                    // `--connect` prints answers from fragments alone, so a late one is printed
                    // late
                    flush(kernel, &mut last, write).await?;
                    if protocol::is_progress(&event) && !progress_recorded {
                        progress(write, last, event).await?;
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
                // note: write the numbered half first, for the reason the event branch gives and a
                // sharper one. `Message::Busy` tells a client the turn is over, and a client whose
                // input has closed leaves on that word. So `busy: false` written before the records
                // of the turn says the turn is done while the last of it is still in the log;
                // `kamchatka --connect` with a question piped in detaches without the answer
                Ok(message) => {
                    caught_up(&mut events, kernel, &mut last, write, progress_recorded).await?;
                    voiced(write, &message).await?;
                }
                // the program's own lines are in no log either, and the same rule applies to them.
                //
                // note: among them is state said only when it changes - whether the session is
                // busy, the model, a network question - so its current value is asked for and said
                // again after the records, as `busy` always is
                Err(broadcast::error::RecvError::Lagged(frames)) => {
                    flush(kernel, &mut last, write).await?;
                    protocol::write(write, &Message::Missed { frames }).await?;
                    let (answer, answered) = oneshot::channel();
                    if asks.send(FromClient::Standing { answer }).is_ok()
                        && let Ok(standing) = answered.await
                    {
                        caught_up(&mut events, kernel, &mut last, write, progress_recorded)
                            .await?;
                        for message in &standing {
                            protocol::write(write, message).await?;
                        }
                    }
                }
                // note: the session has ended, and this is the last thing this connection does:
                // write out whatever the log grew while it was being written to. Which branch sees
                // the end first is a coin toss - `session.finished` is emitted before the channel
                // is dropped, so both are ready at once and `select!` picks either - and the cost
                // is a client left short of the last records of a session with no socket to go back
                // for them on.
                //
                // note: no test fails without this, because in practice the event branch is ready
                // first, but the race is real, so the flush stays
                Err(broadcast::error::RecvError::Closed) => {
                    flush(kernel, &mut last, write).await?;

                    return Ok(());
                }
            },
        }
    }
}

/// A refusal, and the client command it names: `attach`, `version`, or `projection`.
///
/// note: `attach` is mended by attaching afresh. `version` is not mended by anything; a client that
/// reattaches is refused identically and gives up a minute later saying the session has not
/// answered, when it answered at once. `projection` means the session answers but this attach
/// cannot be served, because the projection is larger than the client reads; attaching again gets
/// the identical answer, and the client can only read the records without one. See
/// [`crate::remote::Client`].
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
/// session loop takes both together. See [`Answered`].
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
        relaying,
    } = attach
    else {
        return Err("that is not an attach".to_owned().into());
    };
    // note: a version this session does not know is refused before anything else is read off the
    // message, because the meaning of the rest depends on it. An older one it knows is served - see
    // `protocol::VERSION`
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
                relaying,
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
        // note: the size check `flush` and `answer` both apply, here because a projection is a
        // frame like any other, and this was once the one write that did not ask. A message larger
        // than `MAX_LINE` in the context makes the projection that long. Unlike a record, which can
        // be named and moved past, a projection cannot be skipped, so a client that cannot read it
        // has no session. Sent unchecked, the other end refuses the frame, which closes the
        // connection and reads as a drop; the client spends a minute reattaching to a session that
        // answered every time.
        //
        // note: it is now the rare case. A conversation too long for a frame has been cut down to
        // fit already - see `Attached::abridge` - so what reaches this is a projection too long for
        // reasons that are not its lines: parts nothing cuts, or so many lines that even cut to
        // nothing they do not fit. It is refused by name rather than written and hoped for, and the
        // refusal says that the client can read the records without a projection.
        let projection = Message::Attached(attached);
        let line = protocol::framed(&projection)?;
        if let Some(bytes) = protocol::overlong(&line) {
            return Err(Refused {
                about: "projection",
                error: format!(
                    "this session cannot be attached: its projection is {bytes} bytes even with \
                     its longest lines cut down, more than a client reads in one line ({}). The \
                     records are still there and are read without a projection - `inspect ID` \
                     fetches any one item - but the session at once does not fit in one line",
                    protocol::MAX_LINE
                ),
            });
        }
        protocol::write_frame(write, &line).await?;

        return Ok((seq, voice));
    };

    // note: this is the half that returns an error, and it is first because it catches the silent
    // case. A session restarted at this address has a log of its own, and a watermark from the
    // session before it can be plausible against it - at which point the client reads one session's
    // records under another's conversation with nothing anywhere saying so. A client is not
    // required to name a session; see `Command::Attach`
    let named = kernel.session_name();
    if session.is_some_and(|session| session != named) {
        return Err(format!(
            "you are resuming a session this is not: this one is `{named}`, and attaching with no \
             `since` starts again here"
        )
        .into());
    }
    // note: a client claiming to have seen more than has happened is refused rather than clamped.
    // It has either confused two sessions or made the number up, and quietly starting it from the
    // end would leave it convinced it held a history it never had
    let last = kernel.last_seq();
    if since > last {
        return Err(format!(
            "this session has {last} record(s) and you say you have {since}; attach with no \
             `since` to start again"
        )
        .into());
    }
    // note: refused above without involving the session, and answered here by the session itself,
    // because the answer carries `busy` and only the loop driving the kernel knows it
    let mut last = since;
    let answered = match ask(
        asks,
        client,
        Command::Attach {
            since: Some(since),
            session: None,
            version: None,
            relaying,
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
        voiced(write, standing).await?;
    }
    protocol::write(write, &message).await?;

    Ok((last, voice))
}

/// Says a command could not be done, and ends the connection on it.
///
/// note: named rather than reported as the connection's, so a failure a client can do something
/// about reads as itself. Named one of two ways because what there is to do about the two differs.
/// See [`Refused`] and the first attach in [`attend`].
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
/// note: this is for `Message::Busy`. A client with its input closed leaves when the session goes
/// quiet, and `busy: false` is how it finds out. If that message overtakes the fragments of the
/// turn it is about, the client detaches without the answer. The records cannot stand in for them:
/// the log names what happened and does not copy it, so `context.added` says an assistant turn
/// exists but not what it said. What the model said reaches a client only as `Message::Progress`,
/// and only if it is written first.
///
/// note: neither end is wrong without this: the session records the answer and stops the turn, and
/// the client leaves when told the session has nothing left to do. The fault is only in the order
/// they are written in.
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
                    progress(write, *last, event).await?;
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

/// Writes a fragment, or counts it as missed where it is too long for a frame.
///
/// note: the rule [`flush`] holds records to, for the half of the stream no record holds. A line of
/// a command's output goes out as one fragment, held only to what a tool keeps - far past
/// `MAX_LINE` once JSON has escaped it. A frame the client refuses closes its connection, so one
/// long line threw every client off the session. A fragment is best-effort and the item it is part
/// of arrives whole, so what is said is what a client that fell behind is told.
async fn progress<W: AsyncWrite + Unpin>(
    write: &mut W,
    after: u64,
    event: Event,
) -> Result<(), String> {
    voiced(write, &Message::Progress { after, event }).await
}

/// Writes one of the program's own lines, or says one was missed where it is too long to read.
///
/// note: the rule [`progress`] holds fragments to, for the lines no record holds. A frame the client
/// refuses closes its connection, and these carry text the session did not write: a network
/// question carries the command it is about as the model wrote it.
async fn voiced<W: AsyncWrite + Unpin>(write: &mut W, message: &Message) -> Result<(), String> {
    let line = protocol::framed(message)?;
    match protocol::overlong(&line) {
        Some(_) => protocol::write(write, &Message::Missed { frames: 1 }).await,
        None => protocol::write_frame(write, &line).await,
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
/// has a capacity and drops for whoever falls behind it; the log has neither. This reads from
/// wherever this connection got to, and is correct however long the connection was asleep, however
/// far behind its subscription fell, and whether or not it was here when the record was written.
async fn flush<W: AsyncWrite + Unpin>(
    kernel: &Kernel,
    last: &mut u64,
    write: &mut W,
) -> Result<(), String> {
    // note: checked first because `kernel.last_seq()` reads one number, while `history_since`
    // searches the log and copies what follows it under the lock every emit waits on. This runs for
    // every event, including `model.delta`, on every connection, and most add no record this
    // connection has not already been sent
    if kernel.last_seq() <= *last {
        return Ok(());
    }
    for record in kernel.history_since(*last) {
        let seq = record.seq;
        let line = protocol::framed(&Message::Record(record))?;
        // note: a record the other end would refuse to read is named with `Message::Oversized`,
        // which carries its sequence, rather than sent, so the client takes it as seen and resumes
        // after it. A frame over `MAX_LINE` closes the connection, and because the client resumes
        // by sequence it would return to the same record on every attempt; one `context.replaced`
        // over the limit would lock everybody out for the rest of the session
        match protocol::overlong(&line) {
            Some(bytes) => protocol::write(write, &Message::Oversized { seq, bytes }).await?,
            None => protocol::write_frame(write, &line).await?,
        }
        *last = seq;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of the program's own lines too long for a client to read is said to have been missed,
    /// rather than sent and refused, which closes the connection.
    #[tokio::test]
    async fn a_line_too_long_to_read_is_missed_rather_than_sent() {
        let long = Message::Said {
            speaker: crate::app::Speaker::Note,
            text: "x".repeat(protocol::MAX_LINE + 1),
        };
        let mut written = Vec::new();
        voiced(&mut written, &long).await.expect("written");
        let said: Message = serde_json::from_slice(&written).expect("one frame");
        assert!(matches!(said, Message::Missed { frames: 1 }), "{said:?}");

        let short = Message::Said {
            speaker: crate::app::Speaker::Note,
            text: "hello".to_owned(),
        };
        let mut written = Vec::new();
        voiced(&mut written, &short).await.expect("written");
        let said: Message = serde_json::from_slice(&written).expect("one frame");
        assert!(matches!(said, Message::Said { .. }), "{said:?}");
    }
}
