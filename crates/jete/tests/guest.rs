//! jete-agent in a real guest: boots the current root filesystem under
//! Firecracker with a vsock device, waits for the agent and runs commands
//! through it.
//!
//! Needs root (KVM, the image directory) and an image built with the agent
//! (scripts/build-rootfs), so it is ignored by default. Run it with:
//!
//!     JETE_RUN_SUDO=1 cargo test --test guest -- --ignored --nocapture
//!
//! This boots Firecracker directly, without the jailer. `jete run` replaces
//! that next.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use jete::agent::{self, AGENT_PORT};
use jete_protocol::{Exec, WindowSize};

const KERNEL: &str = "/var/lib/vms/images/kernels/vmlinux";
const ROOTFS: &str = "/var/lib/vms/images/rootfs/ubuntu-26.04.ext4";

/// A booted VM, killed and cleaned up when dropped.
struct Vm {
    dir: PathBuf,
    firecracker: Child,
}

impl Vm {
    fn boot() -> Vm {
        let dir = PathBuf::from(format!("/var/lib/vms/test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let disk = dir.join("root.ext4");
        let copied = Command::new("cp")
            .args(["--reflink=always", ROOTFS])
            .arg(&disk)
            .status()
            .unwrap();
        assert!(copied.success(), "cannot reflink {ROOTFS}");
        let config = format!(
            r#"{{
              "boot-source": {{
                "kernel_image_path": "{KERNEL}",
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw quiet"
              }},
              "drives": [{{
                "drive_id": "root", "path_on_host": "{disk}",
                "is_root_device": true, "is_read_only": false
              }}],
              "machine-config": {{ "vcpu_count": 2, "mem_size_mib": 512 }},
              "vsock": {{ "guest_cid": 3, "uds_path": "{vsock}" }}
            }}"#,
            disk = disk.display(),
            vsock = dir.join("vsock.sock").display(),
        );
        std::fs::write(dir.join("vm.json"), config).unwrap();
        let serial = std::fs::File::create(dir.join("serial.log")).unwrap();
        let firecracker = Command::new("firecracker")
            .args(["--no-api", "--config-file"])
            .arg(dir.join("vm.json"))
            .stdin(Stdio::null())
            .stdout(serial.try_clone().unwrap())
            .stderr(serial)
            .spawn()
            .expect("cannot start firecracker");
        Vm { dir, firecracker }
    }

    fn vsock(&self) -> PathBuf {
        self.dir.join("vsock.sock")
    }

    /// Pings until the agent answers; returns how long that took.
    async fn wait_ready(&self, start: Instant) -> Duration {
        let deadline = start + Duration::from_secs(10);
        loop {
            if let Ok(stream) = agent::connect(&self.vsock(), AGENT_PORT).await
                && agent::ping(stream).await.is_ok()
            {
                return start.elapsed();
            }
            assert!(
                Instant::now() < deadline,
                "agent not ready after 10 s; serial log in {}",
                self.dir.display()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn exec(&self, exec: Exec) -> (i32, String) {
        let stream = agent::connect(&self.vsock(), AGENT_PORT).await.unwrap();
        let mut out = Vec::new();
        let status = agent::exec(stream, exec, &b""[..], &mut out, tokio::io::sink())
            .await
            .unwrap();
        (status.code.unwrap(), String::from_utf8(out).unwrap())
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.firecracker.kill();
        let _ = self.firecracker.wait();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn cmd(argv: &[&str]) -> Exec {
    Exec {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: Vec::new(),
        cwd: None,
        tty: None,
    }
}

#[tokio::test]
#[ignore = "needs root and an image with jete-agent; see the file comment"]
async fn agent_answers_and_runs_commands_in_a_guest() {
    assert!(Path::new(ROOTFS).exists(), "no image at {ROOTFS}");
    let start = Instant::now();
    let vm = Vm::boot();
    let ready = vm.wait_ready(start).await;
    eprintln!("time-to-ready: {} ms", ready.as_millis());

    let (code, out) = vm.exec(cmd(&["uname", "-r"])).await;
    assert!(code == 0 && out.starts_with("6.18.56"), "{out:?}");

    // The guest's own init, not the host's.
    let (_, out) = vm.exec(cmd(&["cat", "/proc/1/comm"])).await;
    assert_eq!(out.trim(), "systemd");

    let (_, out) = vm.exec(cmd(&["id", "-un"])).await;
    assert_eq!(out.trim(), "root");

    let mut tty = cmd(&["sh", "-c", "tty; stty size"]);
    tty.tty = Some(WindowSize {
        rows: 40,
        cols: 120,
    });
    let (_, out) = vm.exec(tty).await;
    assert!(
        out.starts_with("/dev/pts/") && out.contains("40 120"),
        "{out:?}"
    );

    // The rest of boot finishes normally alongside the agent.
    let (code, out) = vm
        .exec(cmd(&["systemctl", "is-system-running", "--wait"]))
        .await;
    assert_eq!((code, out.trim()), (0, "running"));
    let (_, out) = vm.exec(cmd(&["systemd-analyze"])).await;
    eprintln!("{}", out.lines().next().unwrap_or_default());
}
