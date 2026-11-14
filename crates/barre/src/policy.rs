//! Policies: what each VM may reach, one file per VM's host-side address.
//!
//! A policy records the interface that carried the address when it was
//! written, by name and index. It applies only while that same interface
//! exists: when the VM is removed, its interface goes with it, and a new
//! interface given the same address has another index. So a VM removed
//! without `revoke` can't pass its policy on to the next one.

use std::fs;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::rules::Rule;

/// Where policies live. `/run` is cleared at boot, like the VMs.
pub const DIR: &str = "/run/barre/policies";

const IP: &str = "/usr/sbin/ip";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// The VM's id, for the logs.
    pub vm: String,
    /// The interface carrying the address, and its index.
    pub interface: String,
    pub ifindex: u32,
    pub rules: Vec<Rule>,
}

impl Policy {
    /// A policy for the VM behind `addr`, on the interface that carries it
    /// now.
    pub fn new(addr: Ipv4Addr, vm: &str, rules: Vec<Rule>) -> io::Result<Policy> {
        let (interface, ifindex) = interface_of(addr)?;
        Ok(Policy {
            vm: vm.to_string(),
            interface,
            ifindex,
            rules,
        })
    }

    /// Whether the interface it was written for still exists.
    pub fn is_live(&self) -> bool {
        let path = Path::new("/sys/class/net")
            .join(&self.interface)
            .join("ifindex");
        fs::read_to_string(path).is_ok_and(|s| s.trim().parse() == Ok(self.ifindex))
    }
}

fn path(dir: &Path, addr: Ipv4Addr) -> PathBuf {
    dir.join(format!("{addr}.json"))
}

/// Writes `addr`'s policy, replacing any it had. Readable by all: the
/// directory decides who can see it.
pub fn write(dir: &Path, addr: Ipv4Addr, policy: &Policy) -> io::Result<()> {
    let target = path(dir, addr);
    let tmp = dir.join(format!(".{addr}.json.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&tmp)?;
    let mut json = serde_json::to_vec_pretty(policy)?;
    json.push(b'\n');
    file.write_all(&json)?;
    file.sync_all()?;
    fs::rename(&tmp, &target)
}

/// Removes `addr`'s policy. Safe to repeat.
pub fn remove(dir: &Path, addr: Ipv4Addr) -> io::Result<()> {
    match fs::remove_file(path(dir, addr)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Every policy, by address. A file that can't be read is reported, not
/// skipped silently.
pub fn read_all(dir: &Path) -> io::Result<Vec<(Ipv4Addr, Result<Policy, String>)>> {
    let mut all = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(addr) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .and_then(|a| a.parse::<Ipv4Addr>().ok())
        else {
            continue;
        };
        let policy = fs::read(entry.path())
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()));
        all.push((addr, policy));
    }
    all.sort_by_key(|(addr, _)| *addr);
    Ok(all)
}

/// Removes policies whose interface is gone. Returns their addresses.
pub fn prune(dir: &Path) -> io::Result<Vec<Ipv4Addr>> {
    let mut pruned = Vec::new();
    for (addr, policy) in read_all(dir)? {
        if policy.is_ok_and(|p| !p.is_live()) {
            remove(dir, addr)?;
            pruned.push(addr);
        }
    }
    Ok(pruned)
}

/// The interface that has `addr` as one of its own addresses, and its index.
fn interface_of(addr: Ipv4Addr) -> io::Result<(String, u32)> {
    let out = Command::new(IP)
        .args(["-4", "-o", "addr", "show", "to", &addr.to_string()])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("{IP}: {e}")))?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{IP}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let out = String::from_utf8_lossy(&out.stdout);
    parse_ip_addr(&out).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no interface on this host has the address {addr}"),
        )
    })
}

/// The first interface in `ip -o addr` output: `7: jete-ab12cd34    inet ...`.
fn parse_ip_addr(out: &str) -> Option<(String, u32)> {
    let mut words = out.lines().next()?.split_whitespace();
    let index = words.next()?.strip_suffix(':')?.parse().ok()?;
    let name = words.next()?;
    let name = name.split('@').next()?;
    Some((name.to_string(), index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "barre-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn policy(interface: &str, ifindex: u32) -> Policy {
        Policy {
            vm: "3f2a9c1b".into(),
            interface: interface.into(),
            ifindex,
            rules: vec!["github.com:22,443".parse().unwrap()],
        }
    }

    #[test]
    fn policies_are_written_read_and_removed() {
        let dir = dir();
        let addr = Ipv4Addr::new(10, 200, 0, 5);
        write(&dir, addr, &policy("jete-3f2a9c1b", 7)).unwrap();
        fs::write(dir.join("10.200.0.9.json"), "not json").unwrap();
        fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let all = read_all(&dir).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], (addr, Ok(policy("jete-3f2a9c1b", 7))));
        assert!(all[1].1.is_err());
        remove(&dir, addr).unwrap();
        remove(&dir, addr).unwrap();
        assert_eq!(read_all(&dir).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_policy_lives_only_with_its_interface() {
        // `lo` is always index 1.
        assert!(policy("lo", 1).is_live());
        assert!(!policy("lo", 2).is_live());
        assert!(!policy("jete-gone", 1).is_live());
    }

    #[test]
    fn ip_output_names_the_interface() {
        let out = "7: jete-3f2a9c1b    inet 10.200.0.5/30 scope global jete-3f2a9c1b\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(parse_ip_addr(out), Some(("jete-3f2a9c1b".into(), 7)));
        assert_eq!(parse_ip_addr(""), None);
    }
}
