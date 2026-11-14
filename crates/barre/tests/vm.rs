//! barre with real VMs behind jete's egress proxy: a VM reaches what its
//! policy allows, by name, and nothing else.
//!
//! Needs root, jete's host setup (the Ansible `network` role), internet
//! access from the host, and a guest image with curl (scripts/build-rootfs).
//! barre runs inside the test, on its own port, with its own policy
//! directory, so it doesn't disturb an installed barre. Ignored by default;
//! run with:
//!
//!     JETE_RUN_SUDO=1 cargo test -p barre --test vm -- --ignored --nocapture

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use barre::policy::{self, Policy};
use barre::serve::{Config, Server};
use jete::agent::{self, AGENT_PORT};
use jete::net::NetSpec;
use jete::vm::{Host, Spec, Vm};
use jete_protocol::Exec;
use serde_json::Value;

/// The tests run one at a time: one checks which address a new VM gets.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Removes the VM when dropped, so a failing test leaves nothing running.
struct Cleanup(Vm);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.remove();
    }
}

impl Cleanup {
    fn addr(&self) -> Ipv4Addr {
        self.0.network.as_ref().unwrap().host
    }
}

async fn start(port: u16) -> Cleanup {
    let spec = Spec {
        network: Some(NetSpec {
            egress_proxy: Some(port),
            ..NetSpec::default()
        }),
        ..Spec::default()
    };
    let vm = Vm::start(&Host::default(), &spec).await.unwrap();
    eprintln!("{}: time-to-ready {} ms", vm.id, vm.ready_ms.unwrap());
    Cleanup(vm)
}

/// barre on `port`, with a fresh policy directory, collecting its log.
fn barre(port: u16, name: &str) -> (Server, PathBuf, Arc<Mutex<Vec<Value>>>) {
    let dir = std::env::temp_dir().join(format!("barre-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let config = Config {
        policies: dir.clone(),
        log: Arc::new(move |line| {
            eprintln!("barre: {line}");
            sink.lock().unwrap().push(line);
        }),
        ..Config::new(port)
    };
    (Server::new(config), dir, lines)
}

fn allow(dir: &Path, vm: &Cleanup, rules: &[&str]) {
    let rules = rules.iter().map(|r| r.parse().unwrap()).collect();
    let policy = Policy::new(vm.addr(), &vm.0.id, rules).unwrap();
    policy::write(dir, vm.addr(), &policy).unwrap();
}

async fn exec(vm: &Vm, script: &str) -> (i32, String) {
    let stream = agent::connect(&vm.vsock(), AGENT_PORT).await.unwrap();
    let exec = Exec {
        argv: ["timeout", "20", "bash", "-c", script]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        env: Vec::new(),
        cwd: None,
        tty: None,
    };
    let mut out = Vec::new();
    let status = agent::exec(stream, exec, &b""[..], &mut out, tokio::io::sink(), None)
        .await
        .unwrap();
    (status.code.unwrap(), String::from_utf8(out).unwrap())
}

/// The HTTP status curl gets for `url`, or its exit code if it gets none.
async fn curl(vm: &Vm, args: &str) -> Result<String, i32> {
    let (code, out) = exec(
        vm,
        &format!("curl -sS -o /dev/null -w '%{{http_code}}' --max-time 10 {args}"),
    )
    .await;
    if code == 0 { Ok(out) } else { Err(code) }
}

/// Whether barre logged a refusal for `what` with a reason containing
/// `reason`.
fn refused(lines: &Mutex<Vec<Value>>, what: &str, reason: &str) -> bool {
    lines.lock().unwrap().iter().any(|l| {
        l["decision"] == "refuse"
            && l.to_string().contains(what)
            && l["reason"].as_str().unwrap_or_default().contains(reason)
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs root, jete's network and internet; see the file comment"]
async fn a_vm_reaches_what_its_policy_allows_by_name() {
    const PORT: u16 = 47011;
    let _serial = SERIAL.lock().await;
    let (mut barre, dir, log) = barre(PORT, "by-name");
    let guard = start(PORT).await;
    let vm = &guard.0;
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
    let hostname = hostname.trim();
    allow(
        &dir,
        &guard,
        &[
            "example.com",
            "github.com:22",
            &format!("{hostname}:8443"),
            "1.1.1.1",
        ],
    );
    barre.reconcile().await;

    // Allowed: a name over HTTPS, another over SSH, an address.
    assert_eq!(curl(vm, "https://example.com").await, Ok("200".into()));
    let (code, banner) = exec(vm, "exec 3<>/dev/tcp/github.com/22; head -c 8 <&3").await;
    assert_eq!(
        (code, banner.as_str()),
        (0, "SSH-2.0-"),
        "SSH to github.com"
    );
    assert!(
        curl(vm, "https://1.1.1.1").await.is_ok(),
        "an allowed address"
    );

    // Names resolve to synthetic addresses, and only allowed ones resolve.
    let (_, out) = exec(vm, "getent ahostsv4 example.com").await;
    let synthetic = out.split_whitespace().next().unwrap().to_string();
    assert!(synthetic.starts_with("198.18."), "{out}");
    assert_eq!(curl(vm, "https://github.io").await, Err(6), "not resolved");

    // Refused, each with its reason in the log.
    assert!(curl(vm, "http://example.com").await.is_err());
    assert!(refused(&log, "example.com", "not allowed on this port"));
    assert!(curl(vm, "https://8.8.8.8").await.is_err());
    assert!(refused(&log, "8.8.8.8:443", "no rule for this address"));
    let fronted = format!("--resolve github.com:443:{synthetic} https://github.com");
    assert!(curl(vm, &fronted).await.is_err());
    assert!(refused(&log, "github.com", "server name differs"));
    // The host's own name resolves, on the host, to a private address.
    exec(vm, &format!("echo > /dev/tcp/{hostname}/8443")).await;
    assert!(refused(&log, hostname, "non-public"));
    assert!(curl(vm, "https://198.18.200.1").await.is_err());
    assert!(refused(&log, "198.18.200.1", "didn't give this VM"));

    // Every DNS answer is logged.
    let lines = log.lock().unwrap().clone();
    assert!(
        lines
            .iter()
            .any(|l| l["event"] == "dns" && l["answer"] == "NXDOMAIN")
    );
    assert!(
        lines
            .iter()
            .any(|l| l["event"] == "close" && l["name"] == "example.com")
    );

    // Revoked: nothing answers.
    policy::remove(&dir, guard.addr()).unwrap();
    barre.reconcile().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        curl(vm, "https://example.com").await.is_err(),
        "after revoke"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs root, jete's network and internet; see the file comment"]
async fn policies_stay_with_their_vm() {
    const PORT: u16 = 47012;
    let _serial = SERIAL.lock().await;
    let (mut barre, dir, log) = barre(PORT, "per-vm");
    let (a, b) = tokio::join!(start(PORT), start(PORT));
    allow(&dir, &a, &["example.com"]);
    allow(&dir, &b, &["example.org"]);
    barre.reconcile().await;

    // A's synthetic address means nothing to B.
    let (_, out) = exec(&a.0, "getent ahostsv4 example.com").await;
    let synthetic = out.split_whitespace().next().unwrap().to_string();
    let (_, out) = exec(&b.0, "getent ahostsv4 example.com").await;
    assert_eq!(out, "", "B resolved A's name");
    let resolve = format!("--resolve example.com:443:{synthetic} https://example.com");
    assert!(curl(&b.0, &resolve).await.is_err(), "B used A's address");
    assert!(refused(&log, &b.0.id, "didn't give this VM"));
    assert_eq!(curl(&a.0, "https://example.com").await, Ok("200".into()));

    // A is removed without a revoke, and a new VM gets its address: A's
    // policy doesn't apply to it.
    let addr = a.addr();
    a.0.remove().unwrap();
    drop(a);
    barre.reconcile().await;
    let c = start(PORT).await;
    assert_eq!(c.addr(), addr, "the new VM didn't get the freed address");
    barre.reconcile().await;
    assert!(
        curl(&c.0, "https://example.com").await.is_err(),
        "inherited a policy"
    );
    assert!(
        policy::prune(&dir).unwrap().contains(&addr),
        "stale policy kept"
    );

    std::fs::remove_dir_all(dir).unwrap();
}
