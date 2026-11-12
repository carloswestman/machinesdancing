//! VM networks on the host: a VM reaches exactly what it is given.
//!
//! Needs root, IPv4 forwarding (the Ansible `network` role), internet access
//! from the host, and an image whose /etc/resolv.conf points at
//! /proc/net/pnp (scripts/build-rootfs). Ignored by default; run with:
//!
//!     JETE_RUN_SUDO=1 cargo test --test net -- --ignored --nocapture
//!
//! A destination counts as unreachable only if the connection times out:
//! a refused connection would mean the packet got through.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::Command;
use std::time::Duration;

use jete::agent::{self, AGENT_PORT};
use jete::net::{Cidr, NetSpec};
use jete::vm::{Host, Spec, Vm};
use jete_protocol::Exec;

/// Exit code of `timeout` when the command ran out of time.
const TIMED_OUT: i32 = 124;

/// Removes the VM when dropped, so a failing test leaves nothing running.
struct Cleanup(Vm);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.remove();
    }
}

async fn start(network: Option<NetSpec>) -> Cleanup {
    let spec = Spec {
        network,
        ..Spec::default()
    };
    let vm = Vm::start(&Host::default(), &spec).await.unwrap();
    eprintln!("{}: time-to-ready {} ms", vm.id, vm.ready_ms.unwrap());
    Cleanup(vm)
}

fn spec(allow: &[&str], host_ports: &[u16], dns: Option<&str>) -> Option<NetSpec> {
    Some(NetSpec {
        allow: allow.iter().map(|s| s.parse::<Cidr>().unwrap()).collect(),
        host_ports: host_ports.to_vec(),
        dns: dns.map(|s| s.parse().unwrap()),
    })
}

async fn exec(vm: &Vm, argv: &[&str]) -> (i32, String) {
    let stream = agent::connect(&vm.vsock(), AGENT_PORT).await.unwrap();
    let exec = Exec {
        argv: argv.iter().map(|s| s.to_string()).collect(),
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

/// Opens a TCP connection from the guest: 0 if it connects, [`TIMED_OUT`]
/// if nothing answers within 3 seconds.
async fn connect(vm: &Vm, addr: impl std::fmt::Display, port: u16) -> i32 {
    let script = format!("echo > /dev/tcp/{addr}/{port}");
    exec(vm, &["timeout", "3", "bash", "-c", &script]).await.0
}

/// The host's own address and its default gateway, on its default route.
fn host_route() -> (Ipv4Addr, Ipv4Addr) {
    let out = Command::new("/usr/sbin/ip")
        .args(["-4", "route", "get", "1.1.1.1"])
        .output()
        .unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    let field = |name: &str| -> Ipv4Addr {
        let mut words = out.split_whitespace();
        words.find(|w| *w == name);
        words.next().unwrap().parse().unwrap()
    };
    (field("src"), field("via"))
}

#[tokio::test]
#[ignore = "needs root, forwarding and internet; see the file comment"]
async fn a_networked_vm_reaches_only_what_it_is_given() {
    const HOST_PORT: u16 = 47001;
    let guard = start(spec(&["1.1.1.1"], &[HOST_PORT], Some("1.1.1.1"))).await;
    let vm = &guard.0;
    let net = vm.network.clone().unwrap();

    // Configured from the boot line.
    let (_, mac) = exec(vm, &["cat", "/sys/class/net/eth0/address"]).await;
    assert_eq!(mac.trim(), net.guest_mac());
    let (_, resolv) = exec(vm, &["cat", "/etc/resolv.conf"]).await;
    assert!(resolv.contains("nameserver 1.1.1.1"), "{resolv:?}");

    // Allowed: its address, DNS through it, and its host port.
    assert_eq!(connect(vm, "1.1.1.1", 443).await, 0, "allowed address");
    let (code, out) = exec(vm, &["getent", "hosts", "one.one.one.one"]).await;
    assert_eq!(code, 0, "DNS through an allowed resolver: {out}");
    let listener = TcpListener::bind((net.host, HOST_PORT)).unwrap();
    let script = format!("echo hello > /dev/tcp/{}/{HOST_PORT}", net.host);
    let (code, _) = exec(vm, &["timeout", "3", "bash", "-c", &script]).await;
    assert_eq!(code, 0, "allowed host port");
    let mut got = String::new();
    listener
        .accept()
        .unwrap()
        .0
        .read_to_string(&mut got)
        .unwrap();
    assert_eq!(got, "hello\n");

    // Not allowed: other addresses, the host's other ports and addresses,
    // the LAN, the tailnet.
    let (host_lan, gateway) = host_route();
    for (what, addr, port) in [
        ("an address not on the list", "8.8.8.8".to_string(), 443),
        ("the host's SSH, on the VM's link", net.host.to_string(), 22),
        (
            "the host's SSH, on its LAN address",
            host_lan.to_string(),
            22,
        ),
        ("the LAN's gateway", gateway.to_string(), 80),
        ("the tailnet", "100.100.100.100".to_string(), 53),
    ] {
        assert_eq!(connect(vm, &addr, port).await, TIMED_OUT, "reached {what}");
    }

    // Nothing connects into the VM, not even the host.
    let err =
        TcpStream::connect_timeout(&SocketAddr::from((net.guest, 22)), Duration::from_secs(2))
            .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");

    // Removed: no tap or rules left.
    vm.remove().unwrap();
    assert!(!net.exists().unwrap(), "network left behind");
}

#[tokio::test]
#[ignore = "needs root; see the file comment"]
async fn vms_never_reach_each_other() {
    // Even when one is allowed the whole pool.
    let (a, b) = tokio::join!(
        start(spec(&["10.200.0.0/16"], &[], None)),
        start(spec(&[], &[53], None))
    );
    let (a, b) = (&a.0, &b.0);
    let (net_a, net_b) = (a.network.clone().unwrap(), b.network.clone().unwrap());
    assert_ne!(net_a.guest, net_b.guest);
    assert_eq!(connect(a, net_b.guest, 80).await, TIMED_OUT, "A reached B");
    assert_eq!(connect(b, net_a.guest, 80).await, TIMED_OUT, "B reached A");
    // Nor each other's host side.
    assert_eq!(
        connect(a, net_b.host, 53).await,
        TIMED_OUT,
        "A reached B's host port"
    );

    a.remove().unwrap();
    assert!(!net_a.exists().unwrap(), "A's network left behind");
    assert!(net_b.exists().unwrap(), "removing A removed B's network");
}

#[tokio::test]
#[ignore = "needs root; see the file comment"]
async fn a_vm_without_network_options_has_no_network_device() {
    let guard = start(None).await;
    let vm = &guard.0;
    assert!(vm.network.is_none());
    let (_, out) = exec(vm, &["ls", "/sys/class/net"]).await;
    assert_eq!(out.trim(), "lo");
}

#[tokio::test]
#[ignore = "needs root; see the file comment"]
async fn a_networked_vm_that_fails_to_boot_leaves_no_network() {
    let spec = Spec {
        kernel: "/etc/hostname".into(),
        network: spec(&["1.1.1.1"], &[53], None),
        ..Spec::default()
    };
    Vm::start(&Host::default(), &spec).await.unwrap_err();
    let taps: Vec<String> = std::fs::read_dir("/sys/class/net")
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("jete-"))
        .collect();
    // Other tests may run VMs at the same time; theirs are recorded.
    let live: Vec<String> = jete::vm::list(&Host::default())
        .unwrap()
        .into_iter()
        .filter_map(|vm| Some(vm.network?.tap))
        .collect();
    let orphans: Vec<&String> = taps.iter().filter(|tap| !live.contains(tap)).collect();
    assert!(orphans.is_empty(), "taps left behind: {orphans:?}");
}
