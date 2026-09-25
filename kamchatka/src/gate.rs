//! The gate a confined command's network goes through: a seccomp filter that holds every attempt to
//! open an internet socket until the process that spawned the command answers it.
//!
//! note: what it is for is knowing a command wants the network from its *trying*. Landlock refuses
//! a `connect` and tells nobody unprivileged, so from outside a refused call looks like nothing at
//! all - which left reading the command line for program names as the only way to know when to
//! ask. `socket()` for `AF_INET` or `AF_INET6` is the first thing any use of the network does, a
//! DNS lookup included, and seccomp user notification is the mechanism that lets another process
//! see that call and answer it. The filter reads only the call's integer arguments, so there is no
//! address a command could change after the answer, and nothing is decided per destination.
//!
//! note: the one module in this crate that writes `unsafe`. `seccomp(2)` and the notification
//! `ioctl`s have no safe wrapper that does not link the C `libseccomp`, which every target a
//! release ships would then have to build, the static one included. What is unsafe is four system
//! calls and a `prctl` on values this module owns, and taking ownership of the descriptor one of
//! them returns, each with its reason beside it. Everything else - the socket pair, handing a
//! descriptor over, polling - goes through `rustix`, which is safe.
//!
//! note: a filter is a program written against one architecture's system call numbers, and these
//! are x86_64's and aarch64's, which are the two this crate builds for. What reads the command for
//! program names instead is left for a kernel that cannot hold a call - see [`holds`] - and for
//! `--no-sandbox`.

#![allow(unsafe_code)]

use std::{
    io::{self, IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::{
        AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
        SendAncillaryMessage, SendFlags, SocketFlags, SocketType,
    },
};
use tokio::io::unix::AsyncFd;

// note: the audit architecture values from `linux/audit.h`, which `libc` does not carry. A
// filter that compares the call number without first comparing these is the classic way
// round one: the same number means a different call in a 32-bit process
#[cfg(target_arch = "x86_64")]
const NATIVE: u32 = 0xC000_003E;
#[cfg(target_arch = "x86_64")]
const FOREIGN: u32 = 0x4000_0003;
#[cfg(target_arch = "aarch64")]
const NATIVE: u32 = 0xC000_00B7;
#[cfg(target_arch = "aarch64")]
const FOREIGN: u32 = 0x4000_0028;

/// The bit an x32 call carries in its number on an x86_64 kernel.
///
/// note: cleared rather than refused, because x32 shares `socket` and `io_uring_setup` with
/// the native table and differs only in that bit - so a filter that compared the number as it
/// arrived would let `0x40000029` through as a call it has never heard of, and it is `socket`
#[cfg(target_arch = "x86_64")]
const X32: Option<u32> = Some(0x4000_0000);
#[cfg(target_arch = "aarch64")]
const X32: Option<u32> = None;

const SOCKET: u32 = libc::SYS_socket as u32;
const IO_URING_SETUP: u32 = libc::SYS_io_uring_setup as u32;

/// `socket`, and `socketcall` where there is one, in the table of the 32-bit processes this
/// kernel also runs.
///
/// note: `socketcall` multiplexes every socket call behind its first argument and keeps the
/// real arguments in memory, which a filter cannot read. So the family of a `socketcall`
/// socket cannot be known, and one is held or refused whatever it is for: the cost is a
/// question about a 32-bit program's unix socket, which is the right way round to be wrong.
#[cfg(target_arch = "x86_64")]
const FOREIGN_SOCKET: u32 = 359;
#[cfg(target_arch = "x86_64")]
const FOREIGN_SOCKETCALL: Option<u32> = Some(102);
#[cfg(target_arch = "aarch64")]
const FOREIGN_SOCKET: u32 = 281;
#[cfg(target_arch = "aarch64")]
const FOREIGN_SOCKETCALL: Option<u32> = None;
/// `socketcall`'s first argument when what it is asked for is a socket.
const SYS_SOCKET: u32 = 1;
/// `io_uring_setup` is the same number in both tables.
const FOREIGN_IO_URING_SETUP: u32 = 425;

/// Where the parts of `struct seccomp_data` a filter reads are.
///
/// note: the low half of the first argument, and only the low half. `socket` takes its family
/// as an `int` and the kernel drops the upper 32 bits before it looks, so a comparison of the
/// whole 64 bits is one a command walks round by setting a high bit - the family the kernel
/// reads is still `AF_INET`, and the filter has seen something else. Little-endian on both
/// architectures, so the low half is first
const NR: u32 = 0;
const ARCH: u32 = 4;
const ARG0: u32 = 16;

/// A jump target in [`program`], resolved to an offset when the program is put together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Label {
    Native,
    NativeNext,
    Family,
    Inet6,
    Foreign,
    ForeignNumber,
    ForeignFamily,
    ForeignInet6,
    ForeignCall,
    ForeignSocketcall,
    ForeignNext,
    Inet,
    Enosys,
    Allow,
    Kill,
}

/// One instruction, with its jumps named rather than counted.
#[derive(Debug, Clone, Copy)]
enum Op {
    Mark(Label),
    Load(u32),
    And(u32),
    Is(u32, Label, Label),
    Return(u32),
}

/// The filter, as the classic BPF the kernel runs.
///
/// note: every internet socket is held, whether the answer is going to be yes, no or a
/// question, so that the process answering always knows an attempt was made. A refusal made
/// in the kernel reaches nobody, and a command refused a name lookup says `Temporary failure in
/// name resolution`, which reads as a network having trouble rather than a network refused.
///
/// note: `io_uring_setup` is refused as though the kernel had none. A ring opens a socket with
/// an operation of its own rather than a `socket()` call, so a gate that let rings through
/// would be a gate with a door beside it. `ENOSYS` rather than a refusal, because it is what a
/// program that can use a ring is written to fall back from.
///
/// note: an architecture this kernel does not run kills the process. None can reach it - a
/// kernel runs its own table and one 32-bit one - and a filter that allowed what it did not
/// recognise is the shape the classic way round one takes.
fn program() -> Vec<libc::sock_filter> {
    use Label::*;

    let mut ops = vec![
        Op::Load(ARCH),
        Op::Is(NATIVE, Native, Foreign),
        Op::Mark(Native),
        Op::Load(NR),
    ];
    if let Some(x32) = X32 {
        ops.push(Op::And(!x32));
    }
    ops.extend([
        Op::Is(SOCKET, Family, NativeNext),
        Op::Mark(NativeNext),
        Op::Is(IO_URING_SETUP, Enosys, Allow),
        Op::Mark(Family),
        Op::Load(ARG0),
        Op::Is(libc::AF_INET as u32, Inet, Inet6),
        Op::Mark(Inet6),
        Op::Is(libc::AF_INET6 as u32, Inet, Allow),
        // the architecture is still what was loaded: nothing on the way here loaded over it
        Op::Mark(Foreign),
        Op::Is(FOREIGN, ForeignNumber, Kill),
        Op::Mark(ForeignNumber),
        Op::Load(NR),
        Op::Is(FOREIGN_SOCKET, ForeignFamily, ForeignCall),
        // its own copy of the test above rather than a jump back to it, since a filter only
        // jumps forward; a 32-bit call's arguments are the low halves of the same slots
        Op::Mark(ForeignFamily),
        Op::Load(ARG0),
        Op::Is(libc::AF_INET as u32, Inet, ForeignInet6),
        Op::Mark(ForeignInet6),
        Op::Is(libc::AF_INET6 as u32, Inet, Allow),
        Op::Mark(ForeignCall),
    ]);
    if let Some(socketcall) = FOREIGN_SOCKETCALL {
        ops.extend([
            Op::Is(socketcall, ForeignSocketcall, ForeignNext),
            Op::Mark(ForeignSocketcall),
            Op::Load(ARG0),
            Op::Is(SYS_SOCKET, Inet, Allow),
        ]);
    }
    ops.extend([
        Op::Mark(ForeignNext),
        Op::Is(FOREIGN_IO_URING_SETUP, Enosys, Allow),
        Op::Mark(Inet),
        Op::Return(libc::SECCOMP_RET_USER_NOTIF),
        Op::Mark(Enosys),
        Op::Return(libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
        Op::Mark(Allow),
        Op::Return(libc::SECCOMP_RET_ALLOW),
        Op::Mark(Kill),
        Op::Return(libc::SECCOMP_RET_KILL_PROCESS),
    ]);

    assemble(&ops)
}

/// Resolves the labels and writes the instructions out.
fn assemble(ops: &[Op]) -> Vec<libc::sock_filter> {
    let mut at = Vec::new();
    let mut index = 0usize;
    for op in ops {
        match op {
            Op::Mark(label) => at.push((*label, index)),
            _ => index += 1,
        }
    }
    let find = |label: Label| {
        at.iter()
            .find(|(known, _)| *known == label)
            .map(|(_, index)| *index)
            .unwrap_or_else(|| panic!("{label:?} is marked somewhere"))
    };

    let mut code = Vec::new();
    for op in ops {
        let here = code.len();
        let jump = |label: Label| {
            let offset = find(label) - here - 1;
            u8::try_from(offset).expect("the program is short enough to jump across")
        };
        code.push(match *op {
            Op::Mark(_) => continue,
            Op::Load(offset) => filter(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, offset),
            Op::And(mask) => filter(libc::BPF_ALU | libc::BPF_AND | libc::BPF_K, 0, 0, mask),
            Op::Is(value, yes, no) => filter(
                libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
                jump(yes),
                jump(no),
                value,
            ),
            Op::Return(value) => filter(libc::BPF_RET | libc::BPF_K, 0, 0, value),
        });
    }

    code
}

fn filter(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

/// Holds every internet socket this process, and everything it runs, will ask for, and sends
/// the listener that answers them down standard input.
///
/// note: for the child that confines itself, after the ruleset and before the command.
///
/// note: standard input because it is the one descriptor a spawning end hands over by name
/// without a helper standing in front of the command, and the command is given `/dev/null`
/// there before it runs - so nothing downstream of this ever holds the socket.
pub fn hold() -> io::Result<()> {
    let listener = install()?;
    hand_over(io::stdin().as_fd(), &listener)
}

/// Installs the filter, handing back its listener.
fn install() -> io::Result<OwnedFd> {
    let code = program();
    let program = libc::sock_fprog {
        len: u16::try_from(code.len()).expect("a filter this short"),
        filter: code.as_ptr().cast_mut(),
    };

    // note: the arguments as `unsigned long`, which is what the kernel reads them as. Handed
    // through a variadic call as `int`, the upper half of each is whatever the register held,
    // and `PR_SET_NO_NEW_PRIVS` refuses anything but zero in the last three
    let no: libc::c_ulong = 0;
    // SAFETY: `PR_SET_NO_NEW_PRIVS` takes integers and touches no memory of ours. Without
    // privileges a filter may be installed only once it is set, and Landlock has set it already
    // where it applied - so this is the gate not depending on that.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, no, no, no) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `program` points at `code`, which outlives the call, and its length is the
    // length of `code`. The kernel copies the filter before it returns.
    let listener = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &program as *const libc::sock_fprog,
        )
    };
    if listener < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: with `NEW_LISTENER` the call returns a descriptor this process now owns and
    // nothing else holds, opened close-on-exec by the kernel.
    Ok(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(listener as i32) })
}

/// Whether this kernel can hold a call for another process to answer.
///
/// note: asked of the kernel, which answers without installing anything, so any process may
/// ask. `CONTINUE`, the answer that lets a held call go on, is Linux 5.5, and Landlock is 5.13:
/// wherever the shell is confined at all, the one implies the other.
pub fn holds() -> bool {
    let action: u32 = libc::SECCOMP_RET_USER_NOTIF;
    // SAFETY: `GET_ACTION_AVAIL` reads one `u32` through the pointer, which is to a local that
    // outlives the call, and changes nothing.
    unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_GET_ACTION_AVAIL,
            0 as libc::c_uint,
            &action as *const u32,
        ) == 0
    }
}

/// The spawning end of a held gate: the standard input to give the child, and where its
/// listener will arrive.
pub fn pair() -> io::Result<(std::process::Stdio, Arriving)> {
    let (mine, theirs) = rustix::net::socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::io::ioctl_fionbio(&mine, true)?;

    Ok((theirs.into(), Arriving(mine)))
}

/// Sends the listener down a socket, as the one byte and the one descriptor
/// [`Arriving::listener`] expects.
fn hand_over(to: BorrowedFd<'_>, listener: &OwnedFd) -> io::Result<()> {
    let fds = [listener.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::Error::other("no room for the descriptor"));
    }
    rustix::net::sendmsg(to, &[IoSlice::new(b"g")], &mut control, SendFlags::empty())?;

    Ok(())
}

/// Where a held child's listener arrives.
pub struct Arriving(OwnedFd);

impl Arriving {
    /// The listener, once the child has sent it; `None` where the child ended without sending
    /// one, which is a child that never got as far as running the command.
    pub async fn listener(self) -> io::Result<Option<Listener>> {
        let from = AsyncFd::new(self.0)?;
        loop {
            let mut ready = from.readable().await?;
            let read = ready.try_io(|fd| {
                let mut byte = [0u8; 1];
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
                let mut control = RecvAncillaryBuffer::new(&mut space);
                rustix::net::recvmsg(
                    fd,
                    &mut [IoSliceMut::new(&mut byte)],
                    &mut control,
                    RecvFlags::CMSG_CLOEXEC,
                )?;
                // whatever came with the byte; an end closed without sending brings neither
                let listener = control.drain().find_map(|message| match message {
                    RecvAncillaryMessage::ScmRights(mut fds) => fds.next(),
                    _ => None,
                });

                Ok(listener)
            });
            match read {
                Ok(Ok(Some(fd))) => return Ok(Some(Listener(AsyncFd::new(fd)?))),
                Ok(Ok(None)) => return Ok(None),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

/// One attempt a held command made, waiting on an answer.
#[derive(Debug)]
pub struct Attempt(u64);

/// What answers a held command's attempts.
pub struct Listener(AsyncFd<OwnedFd>);

impl Listener {
    /// The next attempt; `None` once no process under the filter is left to make one.
    ///
    /// note: polled before it is received, every time. Receiving waits for a notification and
    /// takes no notice of `O_NONBLOCK`, so a receive with nothing there would block the thread
    /// it runs on for as long as the command runs - and for ever once every process under the
    /// filter has gone, which is what `POLLHUP` says. The readiness tokio holds is a hint to
    /// look again, so it is cleared before each look rather than trusted.
    pub async fn next(&self) -> Option<Attempt> {
        loop {
            let mut polled = [PollFd::new(self.0.get_ref(), PollFlags::IN)];
            let now = Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            poll(&mut polled, Some(&now)).ok()?;
            let seen = polled[0].revents();
            if seen.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
                return None;
            }
            if seen.contains(PollFlags::IN) {
                match self.receive() {
                    Ok(attempt) => return Some(attempt),
                    // the process was killed between the notification and the receive
                    Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
                    Err(_) => return None,
                }
            }
            self.0.readable().await.ok()?.clear_ready();
        }
    }

    fn receive(&self) -> io::Result<Attempt> {
        // SAFETY: an all-zero `seccomp_notif` is a valid value of it - integers throughout -
        // and it is what the kernel requires to be handed.
        let mut notif: libc::seccomp_notif = unsafe { std::mem::zeroed() };
        // SAFETY: `RECV` writes one `seccomp_notif` through the pointer, which is to a local
        // of exactly that type; the descriptor is a listener this owns, polled readable just
        // now, so the call does not wait.
        let done = unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::SECCOMP_IOCTL_NOTIF_RECV,
                &mut notif as *mut libc::seccomp_notif,
            )
        };
        match done {
            0 => Ok(Attempt(notif.id)),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Lets the attempt through, or refuses it with `EACCES`.
    ///
    /// note: a failure is not reported. The one there can be is `ENOENT`, which is the process
    /// having been killed while it waited - by a stopped call, most often - and then there is
    /// nobody left to answer.
    pub fn answer(&self, attempt: Attempt, allow: bool) {
        let response = libc::seccomp_notif_resp {
            id: attempt.0,
            val: 0,
            error: match allow {
                true => 0,
                false => -libc::EACCES,
            },
            flags: match allow {
                true => libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32,
                false => 0,
            },
        };
        // SAFETY: `SEND` reads one `seccomp_notif_resp` through the pointer, which is to a
        // local of exactly that type; the descriptor is a listener this owns.
        let _ = unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::SECCOMP_IOCTL_NOTIF_SEND,
                &response as *const libc::seccomp_notif_resp,
            )
        };
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the filter against one call, the way the kernel does.
    ///
    /// note: the four instructions [`program`] writes and no others, so an instruction it
    /// starts writing that this does not know is a failure here rather than a filter nobody
    /// has run.
    fn run(code: &[libc::sock_filter], arch: u32, nr: u32, arg0: u64) -> u32 {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&nr.to_le_bytes());
        data[4..8].copy_from_slice(&arch.to_le_bytes());
        data[16..24].copy_from_slice(&arg0.to_le_bytes());

        let (mut acc, mut at) = (0u32, 0usize);
        loop {
            let op = code.get(at).expect("every path through the filter returns");
            at += 1;
            match u32::from(op.code) {
                code if code == libc::BPF_LD | libc::BPF_W | libc::BPF_ABS => {
                    let from = op.k as usize;
                    acc = u32::from_le_bytes(data[from..from + 4].try_into().expect("a word"));
                }
                code if code == libc::BPF_ALU | libc::BPF_AND | libc::BPF_K => acc &= op.k,
                code if code == libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K => {
                    at += usize::from(if acc == op.k { op.jt } else { op.jf });
                }
                code if code == libc::BPF_RET | libc::BPF_K => return op.k,
                code => panic!("an instruction this does not run: {code:#x}"),
            }
        }
    }

    const INET: u64 = libc::AF_INET as u64;
    const INET6: u64 = libc::AF_INET6 as u64;
    const UNIX: u64 = libc::AF_UNIX as u64;
    const HELD: u32 = libc::SECCOMP_RET_USER_NOTIF;
    const ALLOWED: u32 = libc::SECCOMP_RET_ALLOW;
    const ABSENT: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;

    /// An internet socket is held, and nothing else is.
    #[test]
    fn an_internet_socket_is_held_and_nothing_else_is() {
        let held = program();

        for family in [INET, INET6] {
            assert_eq!(run(&held, NATIVE, SOCKET, family), HELD, "{family}");
        }
        // a unix socket is the filesystem's, and Landlock's to govern
        assert_eq!(run(&held, NATIVE, SOCKET, UNIX), ALLOWED);
        assert_eq!(run(&held, NATIVE, libc::SYS_close as u32, INET), ALLOWED);
        assert_eq!(run(&held, NATIVE, libc::SYS_connect as u32, INET), ALLOWED);
    }

    /// A family with a high bit set is the family the kernel reads.
    ///
    /// note: `socket` takes an `int`, so the kernel drops the upper half of the register
    /// before it looks. A filter comparing all 64 bits sees `0x1_0000_0002` and lets it
    /// through, and the kernel opens an `AF_INET` socket.
    #[test]
    fn a_family_with_a_high_bit_set_is_the_family_the_kernel_reads() {
        let held = program();

        assert_eq!(run(&held, NATIVE, SOCKET, (1 << 32) | INET), HELD);
        assert_eq!(
            run(&held, NATIVE, SOCKET, (0xffff_ffff << 32) | INET6),
            HELD
        );
    }

    /// A ring is refused as though the kernel had none, since it opens sockets of its own.
    #[test]
    fn a_ring_is_refused_as_though_there_were_none() {
        let held = program();

        assert_eq!(run(&held, NATIVE, IO_URING_SETUP, 0), ABSENT);
        assert_eq!(run(&held, FOREIGN, FOREIGN_IO_URING_SETUP, 0), ABSENT);
    }

    /// A 32-bit process is held to the same gate through its own table.
    #[test]
    fn a_32_bit_process_is_held_to_the_same_gate() {
        let held = program();

        assert_eq!(run(&held, FOREIGN, FOREIGN_SOCKET, INET), HELD);
        assert_eq!(run(&held, FOREIGN, FOREIGN_SOCKET, INET6), HELD);
        assert_eq!(run(&held, FOREIGN, FOREIGN_SOCKET, UNIX), ALLOWED);
        // the native number for `socket` means something else in the 32-bit table
        assert_eq!(run(&held, FOREIGN, SOCKET, INET), ALLOWED);

        if let Some(socketcall) = FOREIGN_SOCKETCALL {
            // a `socketcall` socket's family is in memory, so any socket through it is held
            assert_eq!(run(&held, FOREIGN, socketcall, u64::from(SYS_SOCKET)), HELD);
            // and a `socketcall` that is not a socket is not
            assert_eq!(run(&held, FOREIGN, socketcall, 3), ALLOWED);
        }
    }

    /// An x32 call is the native call with a bit set, and the bit is no way round the gate.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_x32_socket_is_a_socket() {
        let held = program();
        let x32 = X32.expect("x86_64 has x32");

        assert_eq!(run(&held, NATIVE, x32 | SOCKET, INET), HELD);
        assert_eq!(run(&held, NATIVE, x32 | IO_URING_SETUP, 0), ABSENT);
        assert_eq!(run(&held, NATIVE, x32 | SOCKET, UNIX), ALLOWED);
    }

    /// An architecture the filter was not written for is killed rather than let through.
    #[test]
    fn an_architecture_it_does_not_know_is_killed() {
        assert_eq!(
            run(&program(), 0xdead_beef, SOCKET, INET),
            libc::SECCOMP_RET_KILL_PROCESS
        );
    }
}
