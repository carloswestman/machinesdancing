//! Real jailed VMs on the host: boot, use through the agent, remove.
//!
//! Needs root and an image built with the agent (scripts/build-rootfs), so
//! these are ignored by default. Run them with:
//!
//!     JETE_RUN_SUDO=1 cargo test --test vm -- --ignored --nocapture

use std::fs;
use std::path::Path;

use jete::agent::{self, AGENT_PORT};
use jete::vm::{self, Host, Spec, UID_BASE, Vm};
use jete_protocol::{Exec, WindowSize};

/// Removes the VM when dropped, so a failing test leaves nothing running.
struct Cleanup(Vm);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.remove();
    }
}

async fn start() -> Cleanup {
    let vm = Vm::start(&Host::default(), &Spec::default()).await.unwrap();
    eprintln!("{}: time-to-ready {} ms", vm.id, vm.ready_ms.unwrap());
    Cleanup(vm)
}

async fn exec(vm: &Vm, argv: &[&str], tty: Option<WindowSize>) -> (i32, String) {
    let stream = agent::connect(&vm.vsock(), AGENT_PORT).await.unwrap();
    let exec = Exec {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: Vec::new(),
        cwd: None,
        tty,
    };
    let mut out = Vec::new();
    let status = agent::exec(stream, exec, &b""[..], &mut out, tokio::io::sink(), None)
        .await
        .unwrap();
    (status.code.unwrap(), String::from_utf8(out).unwrap())
}

/// Running, as opposed to gone or a zombie waiting for init to reap it.
fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !stat
            .rsplit(')')
            .next()
            .unwrap()
            .trim_start()
            .starts_with('Z')
    })
}

fn proc_status(pid: u32, field: &str) -> String {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|l| {
            l.strip_prefix(&format!("{field}:"))
                .map(|v| v.trim().to_string())
        })
        .unwrap()
}

#[tokio::test]
#[ignore = "needs root and an image with jete-agent; see the file comment"]
async fn a_vm_boots_jailed_runs_commands_and_is_removed_without_a_trace() {
    let guard = start().await;
    let vm = &guard.0;
    let pid = vm.pid.unwrap();
    assert!(vm.is_running());

    // Jailed: its own user, cgroup with a memory cap, PID namespace and root.
    let uid = proc_status(pid, "Uid");
    assert!(uid.starts_with(&format!("{0}\t{0}", vm.uid)), "uid {uid}");
    assert!(vm.uid >= UID_BASE);
    let memory_max = fs::read_to_string(vm.cgroup.join("memory.max")).unwrap();
    assert_eq!(memory_max.trim(), ((512u64 + 128) << 20).to_string());
    let nspid = proc_status(pid, "NSpid");
    assert!(
        nspid.ends_with("\t1"),
        "not PID 1 in its namespace: {nspid}"
    );
    // Its root is the jail (pivoted to, so the link reads "/"): the jail's
    // files are there and the host's are not.
    let root = Path::new("/proc").join(pid.to_string()).join("root");
    assert!(root.join("v.sock").exists() && root.join("rootfs.ext4").exists());
    assert!(!root.join("etc").exists(), "the host's /etc is visible");

    // The guest, through the agent.
    let (code, out) = exec(vm, &["uname", "-r"], None).await;
    assert!(code == 0 && out.starts_with("6.18.56"), "{out:?}");
    let (_, out) = exec(vm, &["cat", "/proc/1/comm"], None).await;
    assert_eq!(out.trim(), "systemd");
    let (_, out) = exec(vm, &["id", "-un"], None).await;
    assert_eq!(out.trim(), "root");
    let size = WindowSize {
        rows: 40,
        cols: 120,
    };
    let (_, out) = exec(vm, &["stty", "size"], Some(size)).await;
    assert_eq!(out.trim(), "40 120");
    let (code, out) = exec(vm, &["systemctl", "is-system-running", "--wait"], None).await;
    assert_eq!((code, out.trim()), (0, "running"));

    // Removed: nothing left behind.
    vm.remove().unwrap();
    assert!(!vm.is_running());
    assert!(!alive(pid), "process {pid} survived");
    assert!(!vm.cgroup.exists(), "cgroup left behind");
    assert!(!vm.chroot.parent().unwrap().exists(), "jail left behind");
    let gone = Vm::load(&Host::default(), &vm.id).unwrap_err();
    assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
    // Removing again is harmless.
    vm.remove().unwrap();
}

#[tokio::test]
#[ignore = "needs root and an image with jete-agent; see the file comment"]
async fn vms_get_their_own_ids_and_users() {
    let (a, b) = tokio::join!(start(), start());
    let (a, b) = (&a.0, &b.0);
    assert_ne!(a.id, b.id);
    assert_ne!(a.uid, b.uid);
    assert_ne!(a.chroot, b.chroot);

    // Each guest has its own disk: a file written in one is not in the other.
    let (code, _) = exec(a, &["touch", "/root/only-in-a"], None).await;
    assert_eq!(code, 0);
    let (code, _) = exec(b, &["test", "-e", "/root/only-in-a"], None).await;
    assert_eq!(code, 1);

    let ids: Vec<String> = vm::list(&Host::default())
        .unwrap()
        .into_iter()
        .map(|vm| vm.id)
        .collect();
    assert!(ids.contains(&a.id) && ids.contains(&b.id), "{ids:?}");

    a.remove().unwrap();
    assert!(b.is_running(), "removing one VM stopped another");
}

#[tokio::test]
#[ignore = "needs root and an image with jete-agent; see the file comment"]
async fn a_vm_that_fails_to_boot_is_cleaned_up() {
    // Not a kernel: Firecracker exits during boot.
    let spec = Spec {
        kernel: "/etc/hostname".into(),
        ..Spec::default()
    };
    let err = Vm::start(&Host::default(), &spec).await.unwrap_err();
    assert!(err.to_string().contains("exited during boot"), "{err}");
    let left: Vec<Vm> = vm::list(&Host::default())
        .unwrap()
        .into_iter()
        .filter(|vm| vm.kernel == Path::new("/etc/hostname"))
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}
