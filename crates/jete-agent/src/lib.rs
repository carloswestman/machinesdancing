//! jete-agent: the guest side of jete's control channel.
//!
//! The binary listens on vsock and hands each connection from the host to
//! [`serve`], which speaks the protocol defined in [`jete_protocol`]. Keeping
//! the session logic here, independent of vsock, lets it be tested over any
//! byte stream.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll, ready};

use jete_protocol::{self as proto, Exec, ExitStatus, Frame, Op, Ready, Signal, WindowSize};
use rustix::fs::{Mode, OFlags};
use rustix::process::Pid;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

const BASE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const HOME: &str = "/root";
const CHUNK: usize = 64 * 1024;

type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

/// Serves one session on `stream`, start to finish.
pub async fn serve<S>(stream: S) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let request = match proto::read_frame(&mut reader).await {
        Ok(Some(Frame::Request(request))) => request,
        Ok(Some(_)) => return fail(&mut writer, "expected a request".into()).await,
        Ok(None) => return Ok(()),
        Err(e) => {
            let _ = fail(&mut writer, e.to_string()).await;
            return Err(e);
        }
    };
    if request.version != proto::VERSION {
        let msg = format!(
            "unsupported protocol version {} (agent speaks {})",
            request.version,
            proto::VERSION
        );
        return fail(&mut writer, msg).await;
    }
    match request.op {
        Op::Ping => proto::write_frame(&mut writer, &Frame::Ready(ready_info())).await,
        Op::Exec(exec) => run(exec, reader, writer).await,
    }
}

async fn fail<W: AsyncWrite + Unpin>(writer: &mut W, msg: String) -> io::Result<()> {
    proto::write_frame(writer, &Frame::Error(msg)).await
}

fn ready_info() -> Ready {
    // /proc/uptime is "<seconds since boot> <idle seconds>".
    let uptime_ms = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |secs| (secs * 1000.0) as u64);
    Ready {
        version: proto::VERSION,
        agent: env!("CARGO_PKG_VERSION").into(),
        uptime_ms,
    }
}

/// A started process and its ends of stdin, stdout and stderr.
struct Spawned {
    child: Child,
    stdin: Writer,
    stdout: Reader,
    /// None with a tty: the terminal merges stderr into stdout.
    stderr: Option<Reader>,
    /// The terminal's master side, for resizing.
    pty: Option<OwnedFd>,
}

async fn run<R, W>(exec: Exec, reader: R, mut writer: W) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let Spawned {
        mut child,
        stdin,
        stdout,
        stderr,
        pty,
    } = match spawn(&exec) {
        Ok(spawned) => spawned,
        Err(e) => {
            let program = exec.argv.first().map_or("", String::as_str);
            return fail(&mut writer, format!("cannot run {program:?}: {e}")).await;
        }
    };
    // The process leads its own process group (or session, with a tty), so
    // signals reach everything it starts.
    let group = child
        .id()
        .and_then(|id| Pid::from_raw(id as i32))
        .expect("a just-spawned child has a pid");

    let (stdin_tx, stdin_rx) = mpsc::channel(16);
    tokio::spawn(pump_stdin(stdin_rx, stdin));
    let control = tokio::spawn(read_control(reader, stdin_tx, group, pty));

    let mut stdout = Some(stdout);
    let mut stderr = stderr;
    let mut out_buf = vec![0u8; CHUNK];
    let mut err_buf = vec![0u8; CHUNK];
    let mut status = None;
    // Done when the process has exited and its output is drained.
    while status.is_none() || stdout.is_some() || stderr.is_some() {
        tokio::select! {
            n = read_some(&mut stdout, &mut out_buf), if stdout.is_some() => match n {
                Some(n) => {
                    let frame = Frame::Stdout(out_buf[..n].to_vec());
                    proto::write_frame(&mut writer, &frame).await?;
                }
                None => stdout = None,
            },
            n = read_some(&mut stderr, &mut err_buf), if stderr.is_some() => match n {
                Some(n) => {
                    let frame = Frame::Stderr(err_buf[..n].to_vec());
                    proto::write_frame(&mut writer, &frame).await?;
                }
                None => stderr = None,
            },
            s = child.wait(), if status.is_none() => status = Some(s?),
        }
    }
    // The process is reaped: stop forwarding signals before its pid can be
    // reused.
    control.abort();

    let status = status.expect("loop ends after the process exits");
    let exit = {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus {
            code: status.code(),
            signal: status.signal(),
        }
    };
    proto::write_frame(&mut writer, &Frame::Exit(exit)).await
}

/// Reads one chunk. None means the stream is finished: end of file, or a
/// terminal whose other side has closed (reads fail with EIO).
async fn read_some(reader: &mut Option<Reader>, buf: &mut [u8]) -> Option<usize> {
    let reader = reader.as_mut()?;
    match reader.read(buf).await {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// Writes host input to the process. Runs apart from the output loop so a
/// process that is not reading its input never stalls its output.
async fn pump_stdin(mut rx: mpsc::Receiver<Vec<u8>>, mut stdin: Writer) {
    while let Some(data) = rx.recv().await {
        if stdin.write_all(&data).await.is_err() {
            // The process closed its stdin; drop the rest.
            break;
        }
    }
    let _ = stdin.shutdown().await;
}

/// Handles the host's frames after the request. If the host disconnects or
/// breaks the protocol, the process group is killed: nothing outlives its
/// session.
async fn read_control<R: AsyncRead + Unpin>(
    mut reader: R,
    stdin_tx: mpsc::Sender<Vec<u8>>,
    group: Pid,
    pty: Option<OwnedFd>,
) {
    let mut stdin_tx = Some(stdin_tx);
    loop {
        match proto::read_frame(&mut reader).await {
            Ok(Some(Frame::Stdin(data))) => {
                if let Some(tx) = &stdin_tx {
                    // Fails once the process closed its stdin; drop the data.
                    let _ = tx.send(data).await;
                }
            }
            // With a tty this only stops forwarding input; the terminal
            // stays open.
            Ok(Some(Frame::StdinEof)) => stdin_tx = None,
            Ok(Some(Frame::Resize(size))) => {
                if let Some(pty) = &pty {
                    let _ = set_window_size(pty, size);
                }
            }
            Ok(Some(Frame::Signal(signal))) => {
                let _ = rustix::process::kill_process_group(group, to_signal(signal));
            }
            Ok(Some(_)) | Ok(None) | Err(_) => {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                return;
            }
        }
    }
}

fn to_signal(signal: Signal) -> rustix::process::Signal {
    use rustix::process::Signal as S;
    match signal {
        Signal::Hup => S::HUP,
        Signal::Int => S::INT,
        Signal::Quit => S::QUIT,
        Signal::Term => S::TERM,
        Signal::Kill => S::KILL,
    }
}

fn spawn(exec: &Exec) -> io::Result<Spawned> {
    let (program, args) = exec
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .env("PATH", BASE_PATH)
        .env("HOME", HOME)
        .env("LANG", "C.UTF-8")
        .envs(exec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(exec.cwd.as_deref().unwrap_or(HOME))
        .kill_on_drop(true);

    let Some(size) = exec.tty else {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        return Ok(Spawned {
            child,
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            stderr: Some(Box::new(stderr)),
            pty: None,
        });
    };

    let (master, slave) = open_pty(size)?;
    cmd.stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe syscalls run between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            // A new session whose controlling terminal is the pty, so the
            // shell gets job control and ^C reaches its foreground job.
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // The command holds our copies of the terminal's slave side. Close them,
    // so reads from the master end once the process and its children have.
    drop(cmd);
    Ok(Spawned {
        child,
        stdin: Box::new(Pty::new(master.try_clone()?)?),
        stdout: Box::new(Pty::new(master.try_clone()?)?),
        stderr: None,
        pty: Some(master),
    })
}

fn open_pty(size: WindowSize) -> io::Result<(OwnedFd, OwnedFd)> {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    set_window_size(&master, size)?;
    Ok((master, slave))
}

fn set_window_size(pty: &OwnedFd, size: WindowSize) -> io::Result<()> {
    let winsize = rustix::termios::Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    Ok(rustix::termios::tcsetwinsize(pty, winsize)?)
}

/// The master side of a pseudo-terminal as an async byte stream.
struct Pty(AsyncFd<OwnedFd>);

impl Pty {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        let flags = rustix::fs::fcntl_getfl(&fd)?;
        rustix::fs::fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;
        // SAFETY: an OwnedFd stays open, and keeps returning the same fd,
        // until it is dropped.
        let fd = unsafe { AsyncFd::register(fd) }.map_err(io::Error::from)?;
        Ok(Pty(fd))
    }
}

impl AsyncRead for Pty {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match guard.try_io(|fd| Ok(rustix::io::read(fd.get_ref(), &mut *unfilled)?)) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for Pty {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            match guard.try_io(|fd| Ok(rustix::io::write(fd.get_ref(), buf)?)) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
