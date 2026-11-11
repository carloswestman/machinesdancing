//! Wire protocol between jete on the host and jete-agent in the guest.
//!
//! One vsock connection carries one session. The host opens it and sends a
//! [`Frame::Request`]; what follows depends on the request:
//!
//! - [`Op::Ping`]: the agent answers [`Frame::Ready`] and closes. The first
//!   successful ping after boot is the time-to-ready signal.
//! - [`Op::Exec`]: the agent starts the process and streams its output as
//!   [`Frame::Stdout`] and [`Frame::Stderr`], ending with one [`Frame::Exit`].
//!   Meanwhile the host may send [`Frame::Stdin`], [`Frame::StdinEof`],
//!   [`Frame::Resize`] and [`Frame::Signal`]. If the host disconnects, the
//!   agent kills the process.
//!
//! Any failure is reported as [`Frame::Error`], after which the agent closes
//! the connection.
//!
//! On the wire a frame is a 4-byte big-endian length, then one byte naming
//! the frame kind, then the payload; the length counts the kind byte and the
//! payload. Data frames carry raw bytes, the rest carry JSON. Frames with an
//! unknown kind, a payload over [`MAX_PAYLOAD`] or malformed JSON are
//! rejected: the protocol is strict so it can be checked.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The vsock port jete-agent listens on.
pub const AGENT_PORT: u32 = 1024;

/// Protocol version. Both sides must agree exactly.
pub const VERSION: u32 = 1;

/// Largest payload either side sends or accepts.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// One message on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    // Host to guest.
    /// The first frame of every session.
    Request(Request),
    /// Bytes for the process's stdin (or terminal input, with a tty).
    Stdin(Vec<u8>),
    /// Closes the process's stdin.
    StdinEof,
    /// Resizes the process's terminal. Ignored without a tty.
    Resize(WindowSize),
    /// Sends a signal to the process's group.
    Signal(Signal),

    // Guest to host.
    /// Answer to a ping.
    Ready(Ready),
    /// Bytes the process wrote to stdout (or its terminal, with a tty).
    Stdout(Vec<u8>),
    /// Bytes the process wrote to stderr. Never sent with a tty.
    Stderr(Vec<u8>),
    /// The process ended. The last frame of an exec session.
    Exit(ExitStatus),
    /// The request failed. The last frame of the session.
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Must equal [`VERSION`].
    pub version: u32,
    pub op: Op,
}

impl Request {
    pub fn new(op: Op) -> Self {
        Request {
            version: VERSION,
            op,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Ping,
    Exec(Exec),
}

/// A process to run in the guest, as root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exec {
    /// Program and arguments. The program is looked up in `PATH`.
    pub argv: Vec<String>,
    /// Added to a minimal environment (`PATH`, `HOME`, `LANG`). Nothing is
    /// inherited from the agent.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Working directory. Defaults to `/root`.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Run under a pseudo-terminal of this size. Without one the process
    /// gets pipes, and stdout and stderr stay separate.
    #[serde(default)]
    pub tty: Option<WindowSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
}

/// The signals the host may send. A closed set: nothing else gets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Hup,
    Int,
    Quit,
    Term,
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    /// The agent's protocol version.
    pub version: u32,
    /// The agent's build version.
    pub agent: String,
    /// Guest uptime when the ping was answered.
    pub uptime_ms: u64,
}

/// How a process ended: exactly one of the two is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitStatus {
    /// Exit code, if it exited.
    pub code: Option<i32>,
    /// Signal number, if a signal killed it.
    pub signal: Option<i32>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

mod kind {
    pub const REQUEST: u8 = 1;
    pub const STDIN: u8 = 2;
    pub const STDIN_EOF: u8 = 3;
    pub const RESIZE: u8 = 4;
    pub const SIGNAL: u8 = 5;
    pub const READY: u8 = 16;
    pub const STDOUT: u8 = 17;
    pub const STDERR: u8 = 18;
    pub const EXIT: u8 = 19;
    pub const ERROR: u8 = 20;
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn to_json<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("protocol types always serialize")
}

fn from_json<'a, T: Deserialize<'a>>(payload: &'a [u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(|e| invalid(format!("malformed frame: {e}")))
}

impl Frame {
    fn encode(&self) -> (u8, Vec<u8>) {
        match self {
            Frame::Request(r) => (kind::REQUEST, to_json(r)),
            Frame::Stdin(b) => (kind::STDIN, b.clone()),
            Frame::StdinEof => (kind::STDIN_EOF, Vec::new()),
            Frame::Resize(w) => (kind::RESIZE, to_json(w)),
            Frame::Signal(s) => (kind::SIGNAL, to_json(s)),
            Frame::Ready(r) => (kind::READY, to_json(r)),
            Frame::Stdout(b) => (kind::STDOUT, b.clone()),
            Frame::Stderr(b) => (kind::STDERR, b.clone()),
            Frame::Exit(e) => (kind::EXIT, to_json(e)),
            Frame::Error(m) => (kind::ERROR, to_json(m)),
        }
    }

    fn decode(kind: u8, payload: Vec<u8>) -> io::Result<Frame> {
        Ok(match kind {
            kind::REQUEST => Frame::Request(from_json(&payload)?),
            kind::STDIN => Frame::Stdin(payload),
            kind::STDIN_EOF if payload.is_empty() => Frame::StdinEof,
            kind::RESIZE => Frame::Resize(from_json(&payload)?),
            kind::SIGNAL => Frame::Signal(from_json(&payload)?),
            kind::READY => Frame::Ready(from_json(&payload)?),
            kind::STDOUT => Frame::Stdout(payload),
            kind::STDERR => Frame::Stderr(payload),
            kind::EXIT => Frame::Exit(from_json(&payload)?),
            kind::ERROR => Frame::Error(from_json(&payload)?),
            _ => return Err(invalid(format!("unknown frame kind {kind}"))),
        })
    }
}

/// Writes one frame. Callers split data larger than [`MAX_PAYLOAD`] into
/// several frames.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> io::Result<()> {
    let (kind, payload) = frame.encode();
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload too large",
        ));
    }
    let len = u32::try_from(payload.len() + 1).expect("bounded by MAX_PAYLOAD");
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.push(kind);
    buf.extend_from_slice(&payload);
    w.write_all(&buf).await?;
    w.flush().await
}

/// Reads one frame. Returns `None` if the peer closed the connection
/// cleanly between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(invalid("empty frame"));
    }
    if len - 1 > MAX_PAYLOAD {
        return Err(invalid("frame payload too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let payload = buf.split_off(1);
    Frame::decode(buf[0], payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn roundtrip(frame: Frame) -> Frame {
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).await.unwrap();
        read_frame(&mut buf.as_slice()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn every_frame_roundtrips() {
        let frames = [
            Frame::Request(Request::new(Op::Ping)),
            Frame::Request(Request::new(Op::Exec(Exec {
                argv: vec!["sh".into(), "-c".into(), "echo hi".into()],
                env: vec![("TERM".into(), "xterm".into())],
                cwd: Some("/tmp".into()),
                tty: Some(WindowSize { rows: 24, cols: 80 }),
            }))),
            Frame::Stdin(b"in\0put".to_vec()),
            Frame::StdinEof,
            Frame::Resize(WindowSize {
                rows: 50,
                cols: 132,
            }),
            Frame::Signal(Signal::Term),
            Frame::Ready(Ready {
                version: VERSION,
                agent: "0.1.0".into(),
                uptime_ms: 812,
            }),
            Frame::Stdout(vec![0xff, 0x00, b'\n']),
            Frame::Stderr(Vec::new()),
            Frame::Exit(ExitStatus {
                code: Some(3),
                signal: None,
            }),
            Frame::Error("no such file".into()),
        ];
        for frame in frames {
            assert_eq!(roundtrip(frame.clone()).await, frame);
        }
    }

    #[tokio::test]
    async fn clean_close_between_frames_is_none() {
        assert_eq!(read_frame(&mut &b""[..]).await.unwrap(), None);
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Frame::Stdout(b"hello".to_vec()))
            .await
            .unwrap();
        buf.truncate(buf.len() - 1);
        assert!(read_frame(&mut buf.as_slice()).await.is_err());
    }

    #[tokio::test]
    async fn rejects_unknown_kind() {
        let buf = [0, 0, 0, 1, 99];
        let err = read_frame(&mut &buf[..]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn rejects_oversized_frame_before_reading_it() {
        let len = (MAX_PAYLOAD as u32 + 2).to_be_bytes();
        let err = read_frame(&mut &len[..]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn rejects_unknown_fields() {
        let payload = br#"{"version":1,"op":"ping","sudo":true}"#;
        let mut buf = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        buf.push(kind::REQUEST);
        buf.extend_from_slice(payload);
        assert!(read_frame(&mut buf.as_slice()).await.is_err());
    }

    #[tokio::test]
    async fn rejects_signals_outside_the_allowed_set() {
        let payload = br#""stop""#;
        let mut buf = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        buf.push(kind::SIGNAL);
        buf.extend_from_slice(payload);
        assert!(read_frame(&mut buf.as_slice()).await.is_err());
    }
}
