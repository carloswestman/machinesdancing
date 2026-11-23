//! Allowlist rules: which names and addresses a VM may reach, on which
//! ports.
//!
//! A rule is written `TARGET[:PORT,PORT...]`, port 443 when none is given:
//!
//! - `github.com`: that exact name;
//! - `*.githubusercontent.com`: its subdomains, at any depth, not the bare
//!   name;
//! - `140.82.112.0/20`: an address block, for clients that don't use DNS.
//!
//! An exact name may also name a credential, `api.example.com@token`, on
//! port 443 only: barre then terminates TLS for that name and sets the
//! credential's header on each request (see `credentials` and `intercept`).

use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The port a rule allows when it names none.
pub const DEFAULT_PORT: u16 = 443;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Name(String),
    /// `*.` and this suffix.
    Subdomains(String),
    Block {
        addr: Ipv4Addr,
        prefix: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    target: Target,
    ports: Vec<u16>,
    credential: Option<String>,
}

impl Rule {
    fn matches_name(&self, name: &str) -> bool {
        match &self.target {
            Target::Name(n) => n == name,
            Target::Subdomains(suffix) => name
                .strip_suffix(suffix.as_str())
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
            Target::Block { .. } => false,
        }
    }

    fn matches_addr(&self, ip: Ipv4Addr) -> bool {
        match self.target {
            Target::Block { addr, prefix } => mask(ip, prefix) == addr,
            _ => false,
        }
    }
}

impl FromStr for Rule {
    type Err = String;

    fn from_str(s: &str) -> Result<Rule, String> {
        let err = |why: &str| format!("bad rule {s:?}: {why}");
        let (s_rule, credential) = match s.split_once('@') {
            Some((rule, cred)) => {
                if !is_credential_name(cred) {
                    return Err(err("credentials are named with letters, digits, - and _"));
                }
                (rule, Some(cred.to_string()))
            }
            None => (s, None),
        };
        let (target, ports) = match s_rule.rsplit_once(':') {
            Some((target, ports)) => {
                let ports = ports
                    .split(',')
                    .map(|p| match p.parse::<u16>() {
                        Ok(0) | Err(_) => Err(err("ports are numbers from 1 to 65535")),
                        Ok(p) => Ok(p),
                    })
                    .collect::<Result<Vec<u16>, String>>()?;
                (target, ports)
            }
            None => (s_rule, vec![DEFAULT_PORT]),
        };
        let mut ports = ports;
        ports.sort_unstable();
        ports.dedup();
        let target = if let Some(suffix) = target.strip_prefix("*.") {
            Target::Subdomains(normalize(suffix).ok_or_else(|| err("not a DNS name"))?)
        } else if target.starts_with(|c: char| c.is_ascii_digit())
            && let Some(block) = parse_block(target)
        {
            let (addr, prefix) = block.map_err(|e| err(&e))?;
            Target::Block { addr, prefix }
        } else {
            Target::Name(normalize(target).ok_or_else(|| err("not a DNS name or address"))?)
        };
        if credential.is_some() {
            if !matches!(target, Target::Name(_)) {
                return Err(err("only an exact name can have a credential"));
            }
            if ports != [DEFAULT_PORT] {
                return Err(err("a name with a credential is reached on port 443 only"));
            }
        }
        Ok(Rule {
            target,
            ports,
            credential,
        })
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.target {
            Target::Name(n) => write!(f, "{n}")?,
            Target::Subdomains(suffix) => write!(f, "*.{suffix}")?,
            Target::Block { addr, prefix: 32 } => write!(f, "{addr}")?,
            Target::Block { addr, prefix } => write!(f, "{addr}/{prefix}")?,
        }
        if self.ports != [DEFAULT_PORT] {
            let ports: Vec<String> = self.ports.iter().map(u16::to_string).collect();
            write!(f, ":{}", ports.join(","))?;
        }
        if let Some(c) = &self.credential {
            write!(f, "@{c}")?;
        }
        Ok(())
    }
}

impl Serialize for Rule {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rule {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Rule, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A VM's allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules(pub Vec<Rule>);

impl Rules {
    /// Whether `name` may be looked up: some rule names it, on any port.
    pub fn allows_name(&self, name: &str) -> bool {
        self.0.iter().any(|r| r.matches_name(name))
    }

    /// Whether `name` may be reached on `port`.
    pub fn allows(&self, name: &str, port: u16) -> bool {
        self.0
            .iter()
            .any(|r| r.matches_name(name) && r.ports.contains(&port))
    }

    /// The credential to set on requests to `name` on `port`, if a rule
    /// gives it one. Exact names only, so the first match is the only one
    /// that can carry it.
    pub fn credential(&self, name: &str, port: u16) -> Option<&str> {
        self.0
            .iter()
            .filter(|r| r.matches_name(name) && r.ports.contains(&port))
            .find_map(|r| r.credential.as_deref())
    }

    /// Whether `ip` may be reached directly on `port`.
    pub fn allows_addr(&self, ip: Ipv4Addr, port: u16) -> bool {
        self.0
            .iter()
            .any(|r| r.matches_addr(ip) && r.ports.contains(&port))
    }
}

/// Whether `s` can name a credential: it becomes a file name.
pub fn is_credential_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A DNS name in the form rules compare: lowercase, without a trailing dot.
/// None if it isn't a valid name.
pub fn normalize(name: &str) -> Option<String> {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    let valid_label = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    (name.len() <= 253 && name.split('.').all(valid_label)).then_some(name)
}

/// Whether `ip` is on the public internet: not private, loopback,
/// link-local, shared (the tailnet), reserved, documentation, benchmarking
/// (barre's synthetic addresses), multicast or broadcast.
pub fn is_public(ip: Ipv4Addr) -> bool {
    const NOT_PUBLIC: [(Ipv4Addr, u8); 14] = [
        (Ipv4Addr::new(0, 0, 0, 0), 8),
        (Ipv4Addr::new(10, 0, 0, 0), 8),
        (Ipv4Addr::new(100, 64, 0, 0), 10),
        (Ipv4Addr::new(127, 0, 0, 0), 8),
        (Ipv4Addr::new(169, 254, 0, 0), 16),
        (Ipv4Addr::new(172, 16, 0, 0), 12),
        (Ipv4Addr::new(192, 0, 0, 0), 24),
        (Ipv4Addr::new(192, 0, 2, 0), 24),
        (Ipv4Addr::new(192, 168, 0, 0), 16),
        (Ipv4Addr::new(198, 18, 0, 0), 15),
        (Ipv4Addr::new(198, 51, 100, 0), 24),
        (Ipv4Addr::new(203, 0, 113, 0), 24),
        (Ipv4Addr::new(224, 0, 0, 0), 4),
        (Ipv4Addr::new(240, 0, 0, 0), 4),
    ];
    !NOT_PUBLIC
        .iter()
        .any(|&(net, prefix)| mask(ip, prefix) == net)
}

fn mask(ip: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let bits = u32::from(ip);
    Ipv4Addr::from(if prefix == 0 {
        0
    } else {
        bits & (u32::MAX << (32 - prefix))
    })
}

/// `addr` or `addr/prefix`. None if it isn't an address at all, so it can
/// be read as a name instead.
fn parse_block(s: &str) -> Option<Result<(Ipv4Addr, u8), String>> {
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let addr: Ipv4Addr = addr.parse().ok()?;
    let prefix = match prefix {
        None => 32,
        Some(p) => match p.parse::<u8>() {
            Ok(p) if p <= 32 => p,
            _ => return Some(Err("prefixes are 0 to 32".into())),
        },
    };
    Some(Ok((mask(addr, prefix), prefix)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(list: &[&str]) -> Rules {
        Rules(list.iter().map(|s| s.parse().unwrap()).collect())
    }

    #[test]
    fn rules_parse_and_print() {
        for (input, printed) in [
            ("github.com", "github.com"),
            ("GitHub.com.", "github.com"),
            ("github.com:443", "github.com"),
            ("github.com:443,22,22", "github.com:22,443"),
            ("*.githubusercontent.com", "*.githubusercontent.com"),
            ("140.82.112.0/20:22", "140.82.112.0/20:22"),
            ("140.82.112.7/20", "140.82.112.0/20"),
            ("1.1.1.1", "1.1.1.1"),
            ("API.anthropic.com@claude", "api.anthropic.com@claude"),
            ("github.com:443@github-git", "github.com@github-git"),
        ] {
            assert_eq!(input.parse::<Rule>().unwrap().to_string(), printed);
        }
    }

    #[test]
    fn bad_rules_are_rejected() {
        for s in [
            "",
            "*",
            "*.",
            "github.com:",
            "github.com:0",
            "github.com:65536",
            "github.com:ssh",
            "git hub.com",
            "-github.com",
            "1.2.3.4/33",
            "a..b",
            "github.com@",
            "github.com@a/b",
            "github.com@..",
            "github.com:22@git",
            "github.com:22,443@git",
            "*.github.com@git",
            "1.2.3.4@ip",
        ] {
            assert!(s.parse::<Rule>().is_err(), "{s:?} accepted");
        }
    }

    #[test]
    fn names_match_exactly_or_as_subdomains() {
        let r = rules(&["github.com", "*.githubusercontent.com:443,80"]);
        assert!(r.allows("github.com", 443));
        assert!(!r.allows("github.com", 22));
        assert!(!r.allows("api.github.com", 443));
        assert!(!r.allows("evilgithub.com", 443));
        assert!(r.allows("raw.githubusercontent.com", 80));
        assert!(r.allows("a.b.githubusercontent.com", 443));
        assert!(!r.allows("githubusercontent.com", 443));
        assert!(!r.allows("xgithubusercontent.com", 443));
        assert!(r.allows_name("raw.githubusercontent.com"));
        assert!(!r.allows_name("example.com"));
    }

    #[test]
    fn credentials_belong_to_their_name() {
        let r = rules(&["api.github.com@github-api", "github.com", "*.github.com"]);
        assert_eq!(r.credential("api.github.com", 443), Some("github-api"));
        assert_eq!(r.credential("github.com", 443), None);
        assert_eq!(r.credential("raw.github.com", 443), None);
        assert_eq!(r.credential("api.github.com", 80), None);
        assert!(r.allows("api.github.com", 443));
    }

    #[test]
    fn address_rules_allow_only_addresses() {
        let r = rules(&["140.82.112.0/20:22"]);
        assert!(r.allows_addr("140.82.127.255".parse().unwrap(), 22));
        assert!(!r.allows_addr("140.82.128.0".parse().unwrap(), 22));
        assert!(!r.allows_addr("140.82.112.1".parse().unwrap(), 443));
        assert!(!r.allows_name("140.82.112.0"));
    }

    #[test]
    fn rules_round_trip_through_json() {
        let r: Rule = "github.com:22,443".parse().unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"github.com:22,443\"");
        assert_eq!(serde_json::from_str::<Rule>(&json).unwrap(), r);
    }

    #[test]
    fn only_public_addresses_are_public() {
        for ip in ["1.1.1.1", "140.82.112.3", "100.63.255.255", "172.32.0.1"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.200.0.1",
            "127.0.1.1",
            "192.168.0.1",
            "100.100.100.100",
            "169.254.169.254",
            "172.16.0.1",
            "198.18.0.1",
            "198.19.255.255",
            "224.0.0.1",
            "255.255.255.255",
            "0.0.0.0",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
    }
}
