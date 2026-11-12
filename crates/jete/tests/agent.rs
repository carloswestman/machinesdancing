//! The host client against the real agent session, in one process, over an
//! in-memory stream. Runs on hemlock (as the admin user, not root), so the
//! processes are real Linux processes; only vsock is left out.

use std::time::Duration;

use jete::agent;
use jete_protocol::{self as proto, Exec, ExitStatus, Frame, Op, Request, Signal, WindowSize};
use tokio::io::DuplexStream;

/// A connected client stream, with the agent serving the other end.
fn session() -> DuplexStream {
    let (client, server) = tokio::io::duplex(256 * 1024);
    tokio::spawn(jete_agent::serve(server));
    client
}

fn cmd(argv: &[&str]) -> Exec {
    Exec {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: Vec::new(),
        // The default, /root, is not readable by the non-root test user.
        cwd: Some("/tmp".into()),
        tty: None,
    }
}

async fn run(exec: Exec, stdin: &'static [u8]) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = agent::exec(session(), exec, stdin, &mut out, &mut err, None)
        .await
        .unwrap();
    (status, out, err)
}

fn exited(code: i32) -> ExitStatus {
    ExitStatus {
        code: Some(code),
        signal: None,
    }
}

#[tokio::test]
async fn ping_reports_versions_and_uptime() {
    let ready = agent::ping(session()).await.unwrap();
    assert_eq!(ready.version, proto::VERSION);
    assert_eq!(ready.agent, env!("CARGO_PKG_VERSION"));
    assert!(ready.uptime_ms > 0);
}

#[tokio::test]
async fn exec_returns_stdout_stderr_and_exit_code() {
    let (status, out, err) = run(cmd(&["sh", "-c", "echo out; echo err >&2; exit 3"]), b"").await;
    assert_eq!(status, exited(3));
    assert_eq!(out, b"out\n");
    assert_eq!(err, b"err\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_streams_large_input_and_output_without_deadlock() {
    // cat echoes input while more is still arriving, so both directions
    // must flow at once. Bigger than every buffer on the way.
    static INPUT: std::sync::LazyLock<Vec<u8>> =
        std::sync::LazyLock::new(|| (0..8 << 20).map(|i: u32| (i % 251) as u8).collect());
    let (status, out, _) = run(cmd(&["cat"]), &INPUT).await;
    assert_eq!(status, exited(0));
    assert!(out == *INPUT, "output differs from input");
}

#[tokio::test]
async fn exec_starts_from_a_minimal_environment() {
    let mut exec = cmd(&["env"]);
    exec.env = vec![("FOO".into(), "bar".into())];
    let (_, out, _) = run(exec, b"").await;
    let mut vars: Vec<_> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| l.split('=').next().unwrap().to_string())
        .collect();
    vars.sort();
    assert_eq!(vars, ["FOO", "HOME", "LANG", "PATH"]);
}

#[tokio::test]
async fn exec_runs_in_the_requested_directory() {
    let mut exec = cmd(&["pwd"]);
    exec.cwd = Some("/usr".into());
    let (_, out, _) = run(exec, b"").await;
    assert_eq!(out, b"/usr\n");
}

#[tokio::test]
async fn exec_with_a_tty_gets_a_terminal_of_the_requested_size() {
    let mut exec = cmd(&["sh", "-c", "tty; stty size; echo err >&2"]);
    exec.tty = Some(WindowSize {
        rows: 30,
        cols: 100,
    });
    let (status, out, err) = run(exec, b"").await;
    assert_eq!(status, exited(0));
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with("/dev/pts/"), "not a terminal: {out:?}");
    assert!(out.contains("30 100\r\n"), "wrong size: {out:?}");
    // A terminal merges stderr into its output.
    assert!(out.contains("err\r\n"), "stderr missing: {out:?}");
    assert!(err.is_empty());
}

#[tokio::test]
async fn resizes_reach_the_terminal() {
    let mut exec = cmd(&["sh", "-c", "sleep 0.5; stty size"]);
    exec.tty = Some(WindowSize {
        rows: 30,
        cols: 100,
    });
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(WindowSize {
        rows: 50,
        cols: 132,
    })
    .await
    .unwrap();
    let mut out = Vec::new();
    agent::exec(session(), exec, &b""[..], &mut out, Vec::new(), Some(rx))
        .await
        .unwrap();
    assert_eq!(out, b"50 132\r\n");
}

#[tokio::test]
async fn exec_of_a_missing_program_is_an_error() {
    let mut out = Vec::new();
    let err = agent::exec(
        session(),
        cmd(&["no-such-program"]),
        &b""[..],
        &mut out,
        Vec::new(),
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("cannot run"), "{err}");
}

#[tokio::test]
async fn exec_of_an_empty_argv_is_an_error() {
    let err = agent::exec(session(), cmd(&[]), &b""[..], Vec::new(), Vec::new(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("empty argv"), "{err}");
}

#[tokio::test]
async fn a_different_protocol_version_is_refused() {
    let mut s = session();
    let mut request = Request::new(Op::Ping);
    request.version = proto::VERSION + 1;
    proto::write_frame(&mut s, &Frame::Request(request))
        .await
        .unwrap();
    let reply = proto::read_frame(&mut s).await.unwrap();
    assert!(
        matches!(&reply, Some(Frame::Error(m)) if m.contains("unsupported protocol version")),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_session_must_start_with_a_request() {
    let mut s = session();
    proto::write_frame(&mut s, &Frame::Stdin(b"hi".to_vec()))
        .await
        .unwrap();
    let reply = proto::read_frame(&mut s).await.unwrap();
    assert!(matches!(reply, Some(Frame::Error(_))), "{reply:?}");
}

#[tokio::test]
async fn signals_reach_the_process() {
    let mut s = session();
    let request = Request::new(Op::Exec(cmd(&["sleep", "100"])));
    proto::write_frame(&mut s, &Frame::Request(request))
        .await
        .unwrap();
    proto::write_frame(&mut s, &Frame::Signal(Signal::Term))
        .await
        .unwrap();
    let reply = proto::read_frame(&mut s).await.unwrap();
    assert_eq!(
        reply,
        Some(Frame::Exit(ExitStatus {
            code: None,
            signal: Some(15),
        }))
    );
}

#[tokio::test]
async fn disconnecting_kills_the_process_and_its_children() {
    let mut s = session();
    // The shell starts a background child and reports its pid.
    let request = Request::new(Op::Exec(cmd(&["sh", "-c", "sleep 100 & echo $!; wait"])));
    proto::write_frame(&mut s, &Frame::Request(request))
        .await
        .unwrap();
    let Some(Frame::Stdout(out)) = proto::read_frame(&mut s).await.unwrap() else {
        panic!("expected the child's pid");
    };
    let pid: u32 = String::from_utf8(out).unwrap().trim().parse().unwrap();
    assert!(alive(pid), "child {pid} is not running");

    drop(s);
    for _ in 0..100 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("child {pid} survived its session");
}

/// Running, and not a zombie waiting to be reaped.
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !stat
            .rsplit(')')
            .next()
            .unwrap()
            .trim_start()
            .starts_with('Z')
    })
}
