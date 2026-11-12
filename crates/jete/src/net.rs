//! VM networking: a link that reaches only what it is given.
//!
//! A networked VM gets a /30 from [`POOL`] (host side `.1`, guest `.2`) and
//! a tap device, `jete-<id>`, in the host's network namespace, owned by the
//! VM's user so the jailed Firecracker can open it. Its traffic is routed
//! and masqueraded out of the host's default route.
//!
//! jete owns one nftables table, `inet jete`, and touches nothing else. Per
//! VM, a forward chain lets it start connections only to its allowed
//! addresses, and an input chain lets it reach only its host ports on `.1`.
//! Everything else from the VM is dropped, including IPv6, connections to
//! other VMs, and connections into the VM from anywhere.
//!
//! The guest kernel configures its interface from the boot line, so the
//! guest runs no network daemon.

use std::collections::HashSet;
use std::fmt;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The addresses VMs get, a /30 each.
pub const POOL: Ipv4Addr = Ipv4Addr::new(10, 200, 0, 0);
pub const POOL_PREFIX: u8 = 16;
/// How many networked VMs can exist at once.
pub const SLOTS: u32 = 1 << (32 - POOL_PREFIX - 2);

const TABLE: &str = "inet jete";
const TAP_PREFIX: &str = "jete-";
const IP: &str = "/usr/sbin/ip";
const NFT: &str = "/usr/sbin/nft";

/// The base table: hooks that send each VM's traffic to its own chains
/// (through maps keyed by its tap), and drop anything to or from a VM that
/// they don't accept. Created once, by the first networked VM.
const BASE_TABLE: &str = r#"
table inet jete {
    map forward_vms { type ifname : verdict; }
    map input_vms { type ifname : verdict; }

    chain forward {
        type filter hook forward priority filter; policy accept;
        oifname "jete-*" ct state established,related accept
        oifname "jete-*" drop
        iifname "jete-*" iifname vmap @forward_vms
        iifname "jete-*" drop
    }

    chain input {
        type filter hook input priority filter; policy accept;
        iifname "jete-*" iifname vmap @input_vms
        iifname "jete-*" drop
    }

    chain output {
        type filter hook output priority filter; policy accept;
        oifname "jete-*" ct state established,related accept
        oifname "jete-*" drop
    }

    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        ip saddr 10.200.0.0/16 oifname != "jete-*" masquerade
    }
}
"#;

/// An IPv4 address or block, such as `1.1.1.1` or `140.82.112.0/20`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: Ipv4Addr,
    pub prefix: u8,
}

impl FromStr for Cidr {
    type Err = String;

    /// Host bits are cleared: `10.1.2.3/8` means `10.0.0.0/8`.
    fn from_str(s: &str) -> Result<Cidr, String> {
        let (addr, prefix) = match s.split_once('/') {
            Some((addr, prefix)) => (
                addr,
                prefix
                    .parse::<u8>()
                    .ok()
                    .filter(|p| *p <= 32)
                    .ok_or_else(|| format!("bad prefix length in {s:?}"))?,
            ),
            None => (s, 32),
        };
        let addr: Ipv4Addr = addr
            .parse()
            .map_err(|_| format!("not an IPv4 address or CIDR block: {s:?}"))?;
        let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
        Ok(Cidr {
            addr: Ipv4Addr::from(u32::from(addr) & mask),
            prefix,
        })
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.prefix == 32 {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", self.addr, self.prefix)
        }
    }
}

impl Serialize for Cidr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cidr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Cidr, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// What a VM's network may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetSpec {
    /// Destinations it may start connections to, any port and protocol.
    pub allow: Vec<Cidr>,
    /// TCP and UDP ports on the host side of its link it may reach.
    pub host_ports: Vec<u16>,
    /// The resolver the guest uses. Reachable only if allowed above.
    pub dns: Option<Ipv4Addr>,
}

/// A VM's network, as recorded in its `vm.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    /// Its /30 in the pool.
    pub slot: u32,
    pub tap: String,
    /// The host side of the link: its gateway, and where host ports are.
    pub host: Ipv4Addr,
    pub guest: Ipv4Addr,
    #[serde(flatten)]
    pub spec: NetSpec,
}

impl Network {
    pub fn new(id: &str, slot: u32, spec: &NetSpec) -> Network {
        assert!(slot < SLOTS, "slot {slot} is outside the pool");
        let base = u32::from(POOL) + slot * 4;
        let mut spec = spec.clone();
        spec.host_ports.sort_unstable();
        spec.host_ports.dedup();
        Network {
            slot,
            tap: format!("{TAP_PREFIX}{id}"),
            host: Ipv4Addr::from(base + 1),
            guest: Ipv4Addr::from(base + 2),
            spec,
        }
    }

    /// A locally administered MAC derived from the guest's address.
    pub fn guest_mac(&self) -> String {
        let [a, b, c, d] = self.guest.octets();
        format!("06:00:{a:02x}:{b:02x}:{c:02x}:{d:02x}")
    }

    /// The kernel's `ip=` boot argument, which configures `eth0` and its
    /// resolver before userspace starts. The resolver ends up in
    /// /proc/net/pnp, which the image's /etc/resolv.conf points at.
    pub fn boot_arg(&self) -> String {
        let mut arg = format!("ip={}::{}:255.255.255.252::eth0:off", self.guest, self.host);
        if let Some(dns) = self.spec.dns {
            arg.push_str(&format!(":{dns}"));
        }
        arg
    }

    fn id(&self) -> &str {
        &self.tap[TAP_PREFIX.len()..]
    }

    /// The commands that add this VM's chains and route its tap to them.
    fn rules(&self) -> String {
        let id = self.id();
        let (tap, host, guest) = (&self.tap, self.host, self.guest);
        let mut r = String::new();
        // From the VM to elsewhere: only from its own address, only to its
        // allowed destinations.
        r += &format!("add chain {TABLE} vm-{id}-forward\n");
        r += &format!("add rule {TABLE} vm-{id}-forward ip saddr != {guest} drop\n");
        for dest in &self.spec.allow {
            r += &format!("add rule {TABLE} vm-{id}-forward ip daddr {dest} accept\n");
        }
        r += &format!("add rule {TABLE} vm-{id}-forward drop\n");
        // From the VM to the host itself: only its host ports, on its link.
        r += &format!("add chain {TABLE} vm-{id}-input\n");
        r += &format!("add rule {TABLE} vm-{id}-input ip saddr != {guest} drop\n");
        if !self.spec.host_ports.is_empty() {
            let ports: Vec<String> = self.spec.host_ports.iter().map(u16::to_string).collect();
            r += &format!(
                "add rule {TABLE} vm-{id}-input ip daddr {host} meta l4proto {{ tcp, udp }} th dport {{ {} }} accept\n",
                ports.join(", ")
            );
        }
        r += &format!("add rule {TABLE} vm-{id}-input drop\n");
        r += &format!("add element {TABLE} forward_vms {{ \"{tap}\" : jump vm-{id}-forward }}\n");
        r += &format!("add element {TABLE} input_vms {{ \"{tap}\" : jump vm-{id}-input }}\n");
        r
    }

    /// The commands that remove what [`Network::rules`] added. Safe to
    /// repeat: `destroy` ignores what is already gone.
    fn teardown_rules(&self) -> String {
        let id = self.id();
        let tap = &self.tap;
        format!(
            "destroy element {TABLE} forward_vms {{ \"{tap}\" }}\n\
             destroy element {TABLE} input_vms {{ \"{tap}\" }}\n\
             destroy chain {TABLE} vm-{id}-forward\n\
             destroy chain {TABLE} vm-{id}-input\n"
        )
    }

    /// Creates the tap, owned by `uid`, and the firewall rules. Call it with
    /// the VM allocation lock held: the first VM creates the base table.
    pub fn create(&self, uid: u32) -> io::Result<()> {
        if !table_exists()? {
            nft(BASE_TABLE)?;
        }
        let uid = uid.to_string();
        run(
            IP,
            &[
                "tuntap", "add", "dev", &self.tap, "mode", "tap", "user", &uid, "group", &uid,
            ],
        )?;
        run(
            IP,
            &[
                "addr",
                "add",
                &format!("{}/30", self.host),
                "dev",
                &self.tap,
            ],
        )?;
        run(IP, &["link", "set", &self.tap, "up"])?;
        nft(&self.rules())
    }

    /// Deletes the tap and the firewall rules. Safe to repeat, and works on
    /// half-created networks.
    pub fn destroy(&self) -> io::Result<()> {
        if Path::new("/sys/class/net").join(&self.tap).exists() {
            run(IP, &["link", "delete", &self.tap])?;
        }
        // No table means no rules: the host has rebooted since.
        if table_exists()? {
            nft(&self.teardown_rules())?;
        }
        Ok(())
    }

    /// Whether anything of this network is left on the host.
    pub fn exists(&self) -> io::Result<bool> {
        if Path::new("/sys/class/net").join(&self.tap).exists() {
            return Ok(true);
        }
        if !table_exists()? {
            return Ok(false);
        }
        let table = output(NFT, &["list", "table", "inet", "jete"])?;
        Ok(table.contains(&format!("vm-{}-", self.id())) || table.contains(&self.tap))
    }
}

/// The lowest slot not in `taken`.
pub fn free_slot(taken: impl IntoIterator<Item = u32>) -> io::Result<u32> {
    let taken: HashSet<u32> = taken.into_iter().collect();
    (0..SLOTS)
        .find(|slot| !taken.contains(slot))
        .ok_or_else(|| io::Error::other("no free VM network addresses"))
}

fn table_exists() -> io::Result<bool> {
    Ok(output(NFT, &["list", "tables", "inet"])?
        .lines()
        .any(|line| line.trim() == "table inet jete"))
}

/// Applies nft commands as one transaction: all or nothing.
fn nft(commands: &str) -> io::Result<()> {
    let mut child = Command::new(NFT)
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("{NFT}: {e}")))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(commands.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "nft failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn run(program: &str, args: &[&str]) -> io::Result<()> {
    output(program, args).map(|_| ())
}

fn output(program: &str, args: &[&str]) -> io::Result<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("{program}: {e}")))?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(s: &str) -> Cidr {
        s.parse().unwrap()
    }

    #[test]
    fn cidrs_parse_and_print() {
        assert_eq!(cidr("1.1.1.1").to_string(), "1.1.1.1");
        assert_eq!(cidr("1.1.1.1/32").to_string(), "1.1.1.1");
        assert_eq!(cidr("140.82.112.0/20").to_string(), "140.82.112.0/20");
        assert_eq!(cidr("0.0.0.0/0").to_string(), "0.0.0.0/0");
    }

    #[test]
    fn host_bits_are_cleared() {
        assert_eq!(cidr("10.1.2.3/8").to_string(), "10.0.0.0/8");
    }

    #[test]
    fn bad_cidrs_are_rejected() {
        for s in [
            "",
            "github.com",
            "1.2.3",
            "1.2.3.4/33",
            "1.2.3.4/",
            "::1",
            "1.2.3.4/x",
        ] {
            assert!(s.parse::<Cidr>().is_err(), "{s:?} accepted");
        }
    }

    #[test]
    fn cidrs_round_trip_through_json() {
        let c = cidr("140.82.112.0/20");
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"140.82.112.0/20\"");
        assert_eq!(serde_json::from_str::<Cidr>(&json).unwrap(), c);
    }

    #[test]
    fn slots_map_to_their_own_slash_30() {
        let spec = NetSpec::default();
        let first = Network::new("0000000a", 0, &spec);
        assert_eq!(
            (first.host, first.guest),
            ("10.200.0.1".parse().unwrap(), "10.200.0.2".parse().unwrap())
        );
        let last = Network::new("0000000b", SLOTS - 1, &spec);
        assert_eq!(last.guest, "10.200.255.254".parse::<Ipv4Addr>().unwrap());
        assert_eq!(last.tap, "jete-0000000b");
        assert!(last.tap.len() <= 15, "interface names are at most 15 bytes");
        assert_eq!(first.guest_mac(), "06:00:0a:c8:00:02");
    }

    #[test]
    fn free_slot_takes_the_lowest_gap() {
        assert_eq!(free_slot([]).unwrap(), 0);
        assert_eq!(free_slot([0, 1, 3]).unwrap(), 2);
    }

    #[test]
    fn boot_arg_configures_eth0_and_its_resolver() {
        let mut spec = NetSpec::default();
        let net = Network::new("0000000a", 1, &spec);
        assert_eq!(
            net.boot_arg(),
            "ip=10.200.0.6::10.200.0.5:255.255.255.252::eth0:off"
        );
        spec.dns = Some("1.1.1.1".parse().unwrap());
        let net = Network::new("0000000a", 1, &spec);
        assert_eq!(
            net.boot_arg(),
            "ip=10.200.0.6::10.200.0.5:255.255.255.252::eth0:off:1.1.1.1"
        );
    }

    #[test]
    fn rules_allow_only_what_was_given() {
        let spec = NetSpec {
            allow: vec![cidr("1.1.1.1"), cidr("140.82.112.0/20")],
            host_ports: vec![443, 53, 443],
            dns: None,
        };
        let rules = Network::new("0000000a", 0, &spec).rules();
        for line in [
            "add rule inet jete vm-0000000a-forward ip saddr != 10.200.0.2 drop",
            "add rule inet jete vm-0000000a-forward ip daddr 1.1.1.1 accept",
            "add rule inet jete vm-0000000a-forward ip daddr 140.82.112.0/20 accept",
            "add rule inet jete vm-0000000a-forward drop",
            "add rule inet jete vm-0000000a-input ip daddr 10.200.0.1 meta l4proto { tcp, udp } th dport { 53, 443 } accept",
            "add rule inet jete vm-0000000a-input drop",
            "add element inet jete forward_vms { \"jete-0000000a\" : jump vm-0000000a-forward }",
        ] {
            assert!(
                rules.lines().any(|l| l == line),
                "missing {line:?} in\n{rules}"
            );
        }
    }

    #[test]
    fn the_base_table_masquerades_the_pool() {
        assert!(BASE_TABLE.contains(&format!("ip saddr {POOL}/{POOL_PREFIX} ")));
    }

    #[test]
    fn no_host_ports_means_no_host_rule() {
        let rules = Network::new("0000000a", 0, &NetSpec::default()).rules();
        assert!(!rules.contains("dport"), "{rules}");
    }
}
