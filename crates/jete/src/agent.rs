//! Host side of the control channel to jete-agent in a guest.
//!
//! Firecracker exposes a VM's vsock as a Unix socket on the host. Opening a
//! connection to a guest port takes a one-line handshake ([`connect`]); after
//! that the stream carries the [`jete_protocol`] session unchanged.

use std::io;
use std::path::Path;

use jete_protocol::{self as proto, Exec, ExitStatus, Frame, Op, Ready, Request};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

pub use jete_protocol::AGENT_PORT;

/// Opens a connection to `port` in the guest through Firecracker's vsock
/// Unix socket at `uds`.
pub async fn connect(uds: &Path, port: u32) -> io::Result<UnixStream> {
    let mut stream = UnixStream::connect(uds).await?;
    stream
        .write_all(format!("CONNECT {port}\n").as_bytes())
        .await?;
    // Firecracker answers "OK <host port>\n", or closes the connection if
    // nothing listens on the guest port. Read byte by byte so nothing past
    // the line is consumed.
    let mut line = Vec::new();
    loop {
        let byte = stream.read_u8().await.map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("nothing listening on guest vsock port {port}"),
                )
            } else {
                e
            }
        })?;
        if byte == b'\n' {
            break;
        }
        line.push(byte);
        if line.len() > 64 {
            return Err(invalid("vsock handshake: reply too long"));
        }
    }
    if !line.starts_with(b"OK ") {
        return Err(invalid(format!(
            "vsock handshake: unexpected reply {:?}",
            String::from_utf8_lossy(&line)
        )));
    }
    Ok(stream)
}

/// Asks the agent whether the guest is ready.
pub async fn ping<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> io::Result<Ready> {
    proto::write_frame(&mut stream, &Frame::Request(Request::new(Op::Ping))).await?;
    match proto::read_frame(&mut stream).await? {
        Some(Frame::Ready(ready)) => Ok(ready),
        other => Err(unexpected(other)),
    }
}

/// Runs a process in the guest without a tty, copying `stdin` to it until
/// end of file and its output to `stdout` and `stderr`, and returns how it
/// ended.
pub async fn exec<S, I, O, E>(
    stream: S,
    exec: Exec,
    mut stdin: I,
    mut stdout: O,
    mut stderr: E,
) -> io::Result<ExitStatus>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    I: AsyncRead + Unpin + Send + 'static,
    O: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    proto::write_frame(&mut writer, &Frame::Request(Request::new(Op::Exec(exec)))).await?;

    // Input runs on its own task so a process that does not read its input
    // never stalls its output.
    let input = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            proto::write_frame(&mut writer, &Frame::Stdin(buf[..n].to_vec())).await?;
        }
        proto::write_frame(&mut writer, &Frame::StdinEof).await?;
        // Keep the write half open: closing it would end the session.
        std::future::pending::<io::Result<()>>().await
    });

    let result = async {
        loop {
            match proto::read_frame(&mut reader).await? {
                Some(Frame::Stdout(data)) => stdout.write_all(&data).await?,
                Some(Frame::Stderr(data)) => stderr.write_all(&data).await?,
                Some(Frame::Exit(status)) => {
                    stdout.flush().await?;
                    stderr.flush().await?;
                    return Ok(status);
                }
                other => return Err(unexpected(other)),
            }
        }
    }
    .await;
    input.abort();
    result
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn unexpected(frame: Option<Frame>) -> io::Error {
    match frame {
        Some(Frame::Error(msg)) => io::Error::other(format!("agent: {msg}")),
        Some(frame) => invalid(format!("unexpected frame from agent: {frame:?}")),
        None => io::Error::new(io::ErrorKind::UnexpectedEof, "agent closed the connection"),
    }
}
