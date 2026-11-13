//! VM lifecycle: start a jailed Firecracker VM, find it again, remove it.
//!
//! Each VM gets:
//!
//! - a random id (8 hex characters);
//! - its own unprivileged user id, from [`UID_BASE`] up, which the jailer
//!   drops Firecracker to;
//! - a jail, `<jailer root>/<firecracker binary name>/<id>/root`, holding a
//!   hard link to the kernel, a reflinked copy of the root filesystem, the
//!   Firecracker config, and the vsock and API sockets;
//! - a cgroup, `/sys/fs/cgroup/jete/<id>`, capping its memory;
//! - a new PID namespace;
//! - optionally, a network link that reaches only what it is given (see
//!   [`crate::net`]);
//! - a state directory, `<state root>/<id>`, with its record (`vm.json`)
//!   and its console and Firecracker log (`console.log`).
//!
//! Removing a VM kills it and deletes all of the above.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::{self, AGENT_PORT};
use crate::net::{self, NetSpec, Network};

/// The first user id given to a VM. Far above regular users and the
/// subordinate ids (100000-165535) that rootless containers use.
pub const UID_BASE: u32 = 300_000;
/// How many VMs can exist at once, for user id allocation.
pub const UID_COUNT: u32 = 65_536;

/// Parent cgroup of every VM's cgroup.
const PARENT_CGROUP: &str = "jete";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Memory the VM process may use beyond the guest's RAM: Firecracker
/// itself, and the host page cache for its disk.
const MEMORY_OVERHEAD_MIB: u64 = 128;

const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw quiet";
const GUEST_CID: u32 = 3;
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Where things live on the host. [`Host::default`] matches the Ansible
/// setup.
#[derive(Debug, Clone)]
pub struct Host {
    pub firecracker: PathBuf,
    pub jailer: PathBuf,
    pub jailer_root: PathBuf,
    pub state_root: PathBuf,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            firecracker: "/usr/local/bin/firecracker".into(),
            jailer: "/usr/local/bin/jailer".into(),
            jailer_root: "/var/lib/vms/jailer".into(),
            state_root: "/var/lib/vms/state".into(),
        }
    }
}

/// What to boot.
#[derive(Debug, Clone)]
pub struct Spec {
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub vcpus: u8,
    pub mem_mib: u32,
    /// None: no network device at all.
    pub network: Option<NetSpec>,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            kernel: "/var/lib/vms/images/kernels/vmlinux".into(),
            rootfs: "/var/lib/vms/images/rootfs/ubuntu-26.04.ext4".into(),
            vcpus: 2,
            mem_mib: 512,
            network: None,
        }
    }
}

/// A VM's record, kept in its state directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vm {
    pub id: String,
    pub uid: u32,
    /// Host pid of the Firecracker process, once started.
    pub pid: Option<u32>,
    /// The jail's root directory.
    pub chroot: PathBuf,
    pub cgroup: PathBuf,
    /// The images it was booted from, resolved.
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub vcpus: u8,
    pub mem_mib: u32,
    pub created_unix_ms: u64,
    #[serde(default)]
    pub network: Option<Network>,
    /// Time from the start request until the agent answered.
    pub ready_ms: Option<u64>,
    #[serde(skip)]
    state_dir: PathBuf,
    #[serde(skip)]
    jail_dir: PathBuf,
}

impl Vm {
    /// Boots a new VM and waits until its agent answers.
    pub async fn start(host: &Host, spec: &Spec) -> io::Result<Vm> {
        let started = Instant::now();
        let mut vm = Vm::prepare(host, spec)?;
        let booted = async {
            vm.launch(host)?;
            vm.wait_ready(started).await
        }
        .await;
        match booted {
            Ok(ready) => {
                vm.ready_ms = Some(ready.as_millis() as u64);
                vm.save()?;
                Ok(vm)
            }
            Err(e) => {
                let console = vm.console_tail();
                let _ = vm.remove();
                Err(io::Error::new(
                    e.kind(),
                    format!("{e}\nlast console output:\n{console}"),
                ))
            }
        }
    }

    /// Loads the record of an existing VM.
    pub fn load(host: &Host, id: &str) -> io::Result<Vm> {
        validate_id(id)?;
        let state_dir = host.state_root.join(id);
        let data = fs::read(state_dir.join("vm.json")).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(io::ErrorKind::NotFound, format!("no VM {id}"))
            } else {
                e
            }
        })?;
        let mut vm: Vm = serde_json::from_slice(&data).map_err(io::Error::other)?;
        vm.state_dir = state_dir;
        vm.jail_dir = vm
            .chroot
            .parent()
            .expect("the chroot is <jail dir>/root")
            .to_path_buf();
        Ok(vm)
    }

    /// The host side of the VM's vsock.
    pub fn vsock(&self) -> PathBuf {
        self.chroot.join("v.sock")
    }

    /// The VM's console and Firecracker log.
    pub fn console_log(&self) -> PathBuf {
        self.state_dir.join("console.log")
    }

    /// Whether its Firecracker process is alive. Checks the process's
    /// cgroup, so a recycled pid is never mistaken for the VM.
    pub fn is_running(&self) -> bool {
        let Some(pid) = self.pid else { return false };
        let expected = format!("0::/{PARENT_CGROUP}/{}\n", self.id);
        fs::read_to_string(format!("/proc/{pid}/cgroup")).is_ok_and(|cg| cg == expected)
    }

    /// Kills the VM if it runs, and deletes its jail, cgroup, network and
    /// state.
    /// Safe to repeat, and works on half-created VMs.
    pub fn remove(&self) -> io::Result<()> {
        self.kill()?;
        // The cgroup empties as the process exits; only then can it go.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fs::remove_dir(&self.cgroup) {
                Ok(()) => break,
                Err(e) if e.kind() == io::ErrorKind::NotFound => break,
                Err(e) if Instant::now() > deadline => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("cannot remove cgroup {}: {e}", self.cgroup.display()),
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        if let Some(network) = &self.network {
            network.destroy()?;
        }
        remove_dir_all_if_exists(&self.jail_dir)?;
        remove_dir_all_if_exists(&self.state_dir)
    }

    /// Kills the Firecracker process, if it runs, and waits until it has
    /// exited. Works through a pidfd, so it can only ever hit this VM's
    /// process, even if its pid has been reused.
    fn kill(&self) -> io::Result<()> {
        use rustix::event::{PollFd, PollFlags, Timespec};
        use rustix::process::{Pid, PidfdFlags, Signal};

        let Some(pid) = self.pid.and_then(|pid| Pid::from_raw(pid as i32)) else {
            return Ok(());
        };
        let Ok(pidfd) = rustix::process::pidfd_open(pid, PidfdFlags::empty()) else {
            return Ok(()); // already gone
        };
        // The pidfd now pins one process. Make sure it is ours.
        if !self.is_running() {
            return Ok(());
        }
        rustix::process::pidfd_send_signal(&pidfd, Signal::KILL)?;
        // A pidfd becomes readable when its process exits.
        let mut fds = [PollFd::new(&pidfd, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        if rustix::event::poll(&mut fds, Some(&timeout))? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("VM {} did not exit after SIGKILL", self.id),
            ));
        }
        Ok(())
    }

    /// Creates the id, state directory, network and jail, without starting
    /// anything.
    fn prepare(host: &Host, spec: &Spec) -> io::Result<Vm> {
        if let Some(network) = &spec.network {
            network
                .check()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        }
        let kernel =
            fs::canonicalize(&spec.kernel).map_err(|e| with_path(e, "kernel", &spec.kernel))?;
        let rootfs = fs::canonicalize(&spec.rootfs)
            .map_err(|e| with_path(e, "root filesystem", &spec.rootfs))?;
        let firecracker = fs::canonicalize(&host.firecracker)
            .map_err(|e| with_path(e, "firecracker", &host.firecracker))?;
        let exec_name = firecracker
            .file_name()
            .expect("a canonical file path has a name");

        fs::create_dir_all(&host.state_root)?;
        // Ids, user ids and network slots are allocated under a lock, so
        // concurrent starts never share any.
        let lock = File::open(&host.state_root)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
        let existing = list(host)?;
        let uid = free_uid(&existing)?;
        let (id, state_dir) = loop {
            let id = random_id()?;
            let dir = host.state_root.join(&id);
            match fs::create_dir(&dir) {
                Ok(()) => break (id, dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let jail_dir = host.jailer_root.join(exec_name).join(&id);
        let network = match &spec.network {
            Some(spec) => {
                let slot = net::free_slot(
                    existing
                        .iter()
                        .filter_map(|vm| Some(vm.network.as_ref()?.slot)),
                )?;
                Some(Network::new(&id, slot, spec))
            }
            None => None,
        };
        let mut vm = Vm {
            uid,
            pid: None,
            chroot: jail_dir.join("root"),
            cgroup: Path::new(CGROUP_ROOT).join(PARENT_CGROUP).join(&id),
            kernel,
            rootfs,
            vcpus: spec.vcpus,
            mem_mib: spec.mem_mib,
            created_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            network,
            ready_ms: None,
            id,
            state_dir,
            jail_dir,
        };
        vm.save()?;
        // The record names the network before it exists, so removing the VM
        // cleans up whatever part of it was created.
        if let Some(network) = &vm.network
            && let Err(e) = network.create(uid)
        {
            let _ = vm.remove();
            return Err(e);
        }
        drop(lock);

        if let Err(e) = vm.fill_jail() {
            let _ = vm.remove();
            return Err(e);
        }
        Ok(vm)
    }

    /// Puts the kernel, disk and Firecracker config into the jail.
    fn fill_jail(&mut self) -> io::Result<()> {
        fs::create_dir_all(&self.chroot)?;
        link_or_copy(&self.kernel, &self.chroot.join("vmlinux"))?;
        reflink(&self.rootfs, &self.chroot.join("rootfs.ext4"), self.uid)?;
        let mut boot_args = BOOT_ARGS.to_string();
        let mut interfaces = Vec::new();
        if let Some(network) = &self.network {
            boot_args.push(' ');
            boot_args.push_str(&network.boot_arg());
            interfaces.push(serde_json::json!({
                "iface_id": "eth0",
                "host_dev_name": network.tap,
                "guest_mac": network.guest_mac(),
            }));
        }
        let config = serde_json::json!({
            "boot-source": {
                "kernel_image_path": "/vmlinux",
                "boot_args": boot_args,
            },
            "drives": [{
                "drive_id": "root",
                "path_on_host": "/rootfs.ext4",
                "is_root_device": true,
                "is_read_only": false,
            }],
            "machine-config": {
                "vcpu_count": self.vcpus,
                "mem_size_mib": self.mem_mib,
            },
            "network-interfaces": interfaces,
            "vsock": {
                "guest_cid": GUEST_CID,
                "uds_path": "/v.sock",
            },
        });
        fs::write(
            self.chroot.join("vm.json"),
            serde_json::to_vec_pretty(&config).map_err(io::Error::other)?,
        )
    }

    /// Starts the jailer, which sets up the jail and execs Firecracker.
    fn launch(&mut self, host: &Host) -> io::Result<()> {
        let firecracker = fs::canonicalize(&host.firecracker)?;
        let exec_name = firecracker.file_name().unwrap().to_owned();
        let console = File::create(self.console_log())?;
        let memory_max = (u64::from(self.mem_mib) + MEMORY_OVERHEAD_MIB) << 20;
        let chroot_base = self
            .jail_dir
            .parent()
            .and_then(Path::parent)
            .expect("jail dir is <jailer root>/<exec name>/<id>");

        let mut jailer = Command::new(&host.jailer);
        jailer
            .arg("--id")
            .arg(&self.id)
            .arg("--exec-file")
            .arg(&firecracker)
            .arg("--uid")
            .arg(self.uid.to_string())
            .arg("--gid")
            .arg(self.uid.to_string())
            .arg("--chroot-base-dir")
            .arg(chroot_base)
            .args(["--cgroup-version", "2", "--parent-cgroup", PARENT_CGROUP])
            .arg("--cgroup")
            .arg(format!("memory.max={memory_max}"))
            .arg("--new-pid-ns")
            .args(["--", "--api-sock", "/api.sock", "--config-file", "/vm.json"])
            .stdin(Stdio::null())
            .stdout(console.try_clone()?)
            .stderr(console);
        // Its own session: the VM outlives this process and is not hit by
        // signals sent to the caller's terminal.
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            jailer.pre_exec(|| {
                rustix::process::setsid()?;
                Ok(())
            });
        }
        // With a new PID namespace, the jailer forks Firecracker into it,
        // writes its host pid to a file, and exits.
        let status = jailer
            .status()
            .map_err(|e| with_path(e, "jailer", &host.jailer))?;
        if !status.success() {
            return Err(io::Error::other(format!("jailer failed: {status}")));
        }
        let pid_file = self
            .chroot
            .join(format!("{}.pid", exec_name.to_string_lossy()));
        let pid = fs::read_to_string(&pid_file)
            .map_err(|e| with_path(e, "pid file", &pid_file))?
            .trim()
            .parse()
            .map_err(|e| io::Error::other(format!("bad pid file: {e}")))?;
        self.pid = Some(pid);
        self.save()
    }

    async fn wait_ready(&self, started: Instant) -> io::Result<Duration> {
        let deadline = started + READY_TIMEOUT;
        loop {
            let attempt = tokio::time::timeout(Duration::from_secs(1), async {
                let stream = agent::connect(&self.vsock(), AGENT_PORT).await?;
                agent::ping(stream).await
            });
            if let Ok(Ok(_)) = attempt.await {
                return Ok(started.elapsed());
            }
            if !self.is_running() {
                return Err(io::Error::other("VM exited during boot"));
            }
            if Instant::now() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("agent not ready after {} s", READY_TIMEOUT.as_secs()),
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn save(&self) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = self.state_dir.join("vm.json.tmp");
        fs::write(&tmp, data)?;
        fs::rename(tmp, self.state_dir.join("vm.json"))
    }

    fn console_tail(&self) -> String {
        let Ok(mut file) = File::open(self.console_log()) else {
            return String::new();
        };
        let len = file.metadata().map_or(0, |m| m.len());
        let _ = file.seek(SeekFrom::Start(len.saturating_sub(4096)));
        let mut buf = Vec::new();
        let _ = file.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    }
}

/// Lists the VMs that have a record, running or not.
pub fn list(host: &Host) -> io::Result<Vec<Vm>> {
    let entries = match fs::read_dir(&host.state_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut vms = Vec::new();
    for entry in entries {
        let id = entry?.file_name();
        if let Some(id) = id.to_str()
            && validate_id(id).is_ok()
            && let Ok(vm) = Vm::load(host, id)
        {
            vms.push(vm);
        }
    }
    vms.sort_by_key(|vm| vm.created_unix_ms);
    Ok(vms)
}

/// The lowest user id no existing VM holds.
fn free_uid(existing: &[Vm]) -> io::Result<u32> {
    let taken: std::collections::HashSet<u32> = existing.iter().map(|vm| vm.uid).collect();
    (UID_BASE..UID_BASE + UID_COUNT)
        .find(|uid| !taken.contains(uid))
        .ok_or_else(|| io::Error::other("no free VM user ids"))
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 4];
    rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Ids are 8 lowercase hex characters. Checking them keeps a caller's id
/// from naming a path outside the state directory.
fn validate_id(id: &str) -> io::Result<()> {
    if id.len() == 8
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a VM id: {id:?}"),
        ))
    }
}

/// A copy-on-write copy of `src`, writable only by `uid`. Fails rather than
/// falling back to a full copy: the storage is set up for reflinks, so a
/// failure means something is wrong.
fn reflink(src: &Path, dest: &Path, uid: u32) -> io::Result<()> {
    let src_file = File::open(src)?;
    let dest_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)?;
    rustix::fs::ioctl_ficlone(&dest_file, &src_file).map_err(|e| {
        io::Error::new(
            io::Error::from(e).kind(),
            format!(
                "cannot reflink {} to {}: {e}",
                src.display(),
                dest.display()
            ),
        )
    })?;
    let owner = rustix::fs::Uid::from_raw(uid);
    let group = rustix::fs::Gid::from_raw(uid);
    rustix::fs::fchown(&dest_file, Some(owner), Some(group))?;
    Ok(())
}

/// Hard-links a read-only file into the jail, or copies it if it lives on
/// another filesystem.
fn link_or_copy(src: &Path, dest: &Path) -> io::Result<()> {
    match fs::hard_link(src, dest) {
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
            fs::copy(src, dest).map(|_| ())
        }
        result => result,
    }
}

fn remove_dir_all_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn with_path(e: io::Error, what: &str, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{what} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_eight_hex_characters() {
        let id = random_id().unwrap();
        assert!(validate_id(&id).is_ok(), "{id}");
    }

    #[test]
    fn ids_that_could_escape_are_rejected() {
        for id in ["../etc00", "ABCDEF12", "abc", "abcdef123", "abcdefg1", ""] {
            assert!(validate_id(id).is_err(), "{id:?} accepted");
        }
    }
}
