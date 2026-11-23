//! The service: for each live policy, listen on its address and decide
//! every DNS query and connection from the VM behind it.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Semaphore, watch};

use crate::ca::Ca;
use crate::credentials;
use crate::dns::{self, Answer};
use crate::intercept::{self, Carry};
use crate::peek::{self, Peek};
use crate::policy::{self, Policy};
use crate::rules::{self, Rules};

/// Where barre's synthetic addresses come from: a range reserved for
/// benchmarking, never used on the internet.
pub const SYNTHETIC: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 0);
const SYNTHETIC_PREFIX: u8 = 15;

/// The most a client may send before naming its destination.
const PEEK_LIMIT: usize = 16 * 1024;

/// One line of the log, as JSON.
pub type Log = Arc<dyn Fn(Value) + Send + Sync>;

/// Prints each log line to standard output, which the journal collects.
pub fn stdout_log() -> Log {
    Arc::new(|line| println!("{line}"))
}

pub struct Config {
    /// The port VMs' traffic is redirected to (jete's `--egress-proxy`).
    pub port: u16,
    pub policies: PathBuf,
    /// Where credentials are kept (see `credentials`).
    pub credentials: PathBuf,
    /// Signs certificates for names with credentials. Without it, those
    /// names are refused.
    pub ca: Option<Arc<Ca>>,
    /// Connections open at once, per VM.
    pub connections: usize,
    /// A connection with no traffic either way for this long is closed.
    pub idle: Duration,
    /// A client must name its destination within this long.
    pub handshake: Duration,
    pub log: Log,
}

impl Config {
    pub fn new(port: u16) -> Config {
        Config {
            port,
            policies: PathBuf::from(policy::DIR),
            credentials: PathBuf::from(credentials::DIR),
            ca: None,
            connections: 256,
            idle: Duration::from_secs(600),
            handshake: Duration::from_secs(10),
            log: stdout_log(),
        }
    }
}

/// Runs until the process ends, picking up policy changes twice a second.
pub async fn serve(config: Config) -> io::Result<()> {
    let mut server = Server::new(config);
    loop {
        server.reconcile().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub struct Server {
    config: Arc<Config>,
    vms: HashMap<Ipv4Addr, Running>,
}

struct Running {
    vm: Arc<Vm>,
    ifindex: u32,
    /// Dropping it stops the VM's listeners and closes its connections.
    _stop: watch::Sender<()>,
}

/// A VM, as barre sees it: an address and a policy.
struct Vm {
    id: String,
    /// Its host-side address, where it reaches barre.
    addr: Ipv4Addr,
    rules: RwLock<Rules>,
    names: Mutex<Synthetic>,
    slots: Arc<Semaphore>,
}

impl Vm {
    /// The guest's address: next to the host side, in jete's /30s.
    fn guest(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.addr) + 1)
    }

    fn log(&self, config: &Config, mut line: Value) {
        line["vm"] = json!(self.id);
        line["addr"] = json!(self.addr);
        (config.log)(line);
    }
}

impl Server {
    pub fn new(config: Config) -> Server {
        Server {
            config: Arc::new(config),
            vms: HashMap::new(),
        }
    }

    /// Brings the listeners in line with the policies on disk: starts new
    /// ones, updates changed rules, and stops those revoked or whose
    /// address is gone.
    pub async fn reconcile(&mut self) {
        let config = self.config.clone();
        let mut wanted = HashMap::new();
        match policy::read_all(&config.policies) {
            Ok(all) => {
                for (addr, policy) in all {
                    match policy {
                        Ok(p) if p.is_live() => {
                            wanted.insert(addr, p);
                        }
                        Ok(_) => {}
                        Err(e) => (config.log)(json!({
                            "event": "policy", "addr": addr, "decision": "ignore",
                            "reason": format!("unreadable: {e}"),
                        })),
                    }
                }
            }
            Err(e) => (config.log)(json!({
                "event": "policy", "decision": "ignore",
                "reason": format!("can't read {}: {e}", config.policies.display()),
            })),
        }

        self.vms.retain(|addr, running| {
            let keep = wanted
                .get(addr)
                .is_some_and(|p| p.ifindex == running.ifindex);
            if !keep {
                running.vm.log(
                    &config,
                    json!({"event": "policy", "decision": "stop",
                           "reason": if wanted.contains_key(addr) { "interface replaced" } else { "revoked or address gone" }}),
                );
            }
            keep
        });

        for (addr, policy) in wanted {
            let rules = Rules(policy.rules.clone());
            if let Some(running) = self.vms.get(&addr) {
                let mut current = running.vm.rules.write().unwrap();
                if *current != rules {
                    *current = rules;
                    running
                        .vm
                        .log(&config, json!({"event": "policy", "decision": "update"}));
                }
                continue;
            }
            match start(&config, addr, &policy, rules).await {
                Ok(running) => {
                    running
                        .vm
                        .log(&config, json!({"event": "policy", "decision": "start"}));
                    self.vms.insert(addr, running);
                }
                Err(e) => (config.log)(json!({
                    "event": "policy", "vm": policy.vm, "addr": addr,
                    "decision": "ignore", "reason": format!("can't listen: {e}"),
                })),
            }
        }
    }
}

async fn start(
    config: &Arc<Config>,
    addr: Ipv4Addr,
    policy: &Policy,
    rules: Rules,
) -> io::Result<Running> {
    let tcp = TcpListener::bind((addr, config.port)).await?;
    let udp = UdpSocket::bind((addr, config.port)).await?;
    let vm = Arc::new(Vm {
        id: policy.vm.clone(),
        addr,
        rules: RwLock::new(rules),
        names: Mutex::new(Synthetic::default()),
        slots: Arc::new(Semaphore::new(config.connections)),
    });
    let (stop, stopped) = watch::channel(());
    tokio::spawn(accept(config.clone(), vm.clone(), tcp, stopped.clone()));
    tokio::spawn(answer_dns(config.clone(), vm.clone(), udp, stopped));
    Ok(Running {
        vm,
        ifindex: policy.ifindex,
        _stop: stop,
    })
}

async fn accept(
    config: Arc<Config>,
    vm: Arc<Vm>,
    listener: TcpListener,
    mut stopped: watch::Receiver<()>,
) {
    loop {
        let (stream, peer) = tokio::select! {
            _ = stopped.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok(a) => a,
                Err(e) => {
                    vm.log(&config, json!({"event": "connect", "decision": "refuse", "reason": format!("accept: {e}")}));
                    continue;
                }
            },
        };
        let (config, vm, mut stopped) = (config.clone(), vm.clone(), stopped.clone());
        tokio::spawn(async move {
            tokio::select! {
                _ = stopped.changed() => {}
                _ = connection(&config, &vm, stream, peer) => {}
            }
        });
    }
}

/// Decides one connection, and carries it if allowed. Logs it either way.
async fn connection(config: &Config, vm: &Vm, mut client: TcpStream, peer: SocketAddr) {
    let started = Instant::now();
    let mut line = json!({"event": "connect"});
    let Ok(_permit) = vm.slots.clone().try_acquire_owned() else {
        line["decision"] = json!("refuse");
        line["reason"] = json!("too many connections");
        vm.log(config, line);
        return;
    };
    let decision = decide(config, vm, &mut client, peer, &mut line).await;
    let (upstream, head, credential) = match decision {
        Ok(go) => go,
        Err(reason) => {
            line["decision"] = json!("refuse");
            line["reason"] = json!(reason);
            line["ms"] = json!(started.elapsed().as_millis() as u64);
            vm.log(config, line);
            return;
        }
    };
    line["decision"] = json!("allow");
    if let Some(credential) = credential {
        line["credential"] = json!(credential);
        vm.log(config, line.clone());
        let why = carry(config, vm, client, head, upstream, &line, &credential).await;
        line["event"] = json!("close");
        line["ms"] = json!(started.elapsed().as_millis() as u64);
        line["reason"] = json!(why);
        vm.log(config, line);
        return;
    }
    vm.log(config, line.clone());
    let (up, down, why) = pipe(client, upstream, &head, config.idle).await;
    line["event"] = json!("close");
    line["bytes_up"] = json!(up);
    line["bytes_down"] = json!(down);
    line["ms"] = json!(started.elapsed().as_millis() as u64);
    line["reason"] = json!(why);
    vm.log(config, line);
}

/// Carries a connection to a name with a credential, through `intercept`:
/// barre reads its requests and sets the credential's header.
async fn carry(
    config: &Config,
    vm: &Vm,
    client: TcpStream,
    head: Vec<u8>,
    upstream: TcpStream,
    line: &Value,
    credential: &str,
) -> String {
    let name = line["name"].as_str().unwrap_or_default().to_string();
    let Some(ca) = &config.ca else {
        return "no certificate authority: run barre ca init".into();
    };
    let tls = match ca.server_config(&name) {
        Ok(t) => t,
        Err(e) => return format!("can't make a certificate: {e}"),
    };
    let read = || credentials::read(&config.credentials, vm.addr, credential);
    let log = |l: Value| vm.log(config, l);
    intercept::carry(
        Carry {
            name: &name,
            tls,
            credential: &read,
            log: &log,
            handshake: config.handshake,
            idle: config.idle,
        },
        client,
        head,
        upstream,
    )
    .await
}

/// Where a connection may go: an open connection there, the bytes already
/// read from the client, and the credential to set on its requests, if its
/// rule names one. Err is the reason it may not.
async fn decide(
    config: &Config,
    vm: &Vm,
    client: &mut TcpStream,
    peer: SocketAddr,
    line: &mut Value,
) -> Result<(TcpStream, Vec<u8>, Option<String>), String> {
    if peer.ip() != vm.guest() {
        line["peer"] = json!(peer);
        return Err("not from the VM's address".into());
    }
    let dest = rustix::net::sockopt::ip_original_dst(&*client)
        .map_err(|e| format!("no original destination: {e}"))?;
    let (ip, port) = (*dest.ip(), dest.port());
    line["dest"] = json!(dest);
    if ip == vm.addr && port == config.port {
        return Err("addressed to barre itself".into());
    }

    if !in_synthetic(ip) {
        if !vm.rules.read().unwrap().allows_addr(ip, port) {
            return Err("no rule for this address and port".into());
        }
        let upstream = connect_to(&[dest], config.handshake).await?;
        return Ok((upstream, Vec::new(), None));
    }

    let name = vm
        .names
        .lock()
        .unwrap()
        .name(ip)
        .ok_or("an address barre didn't give this VM")?;
    line["name"] = json!(name);
    let credential = {
        let rules = vm.rules.read().unwrap();
        if !rules.allows(&name, port) {
            return Err("name not allowed on this port".into());
        }
        rules.credential(&name, port).map(str::to_string)
    };

    // What the client says it wants must be the name it looked up.
    let head = match port {
        443 => {
            let (said, head) = read_name(client, peek::sni, config.handshake).await?;
            if said != name {
                line["sni"] = json!(said);
                return Err("TLS server name differs from the name looked up".into());
            }
            head
        }
        80 => {
            let (said, head) = read_name(client, peek::http_host, config.handshake).await?;
            if said != name {
                line["host"] = json!(said);
                return Err("HTTP Host differs from the name looked up".into());
            }
            head
        }
        _ => Vec::new(),
    };

    let found: Vec<SocketAddrV4> = tokio::net::lookup_host((name.as_str(), port))
        .await
        .map_err(|e| format!("doesn't resolve: {e}"))?
        .filter_map(|a| match a {
            SocketAddr::V4(a) => Some(a),
            SocketAddr::V6(_) => None,
        })
        .collect();
    let public: Vec<SocketAddrV4> = found
        .iter()
        .copied()
        .filter(|a| rules::is_public(*a.ip()))
        .collect();
    if public.is_empty() {
        return Err(if found.is_empty() {
            "no IPv4 address".into()
        } else {
            "resolves only to non-public addresses".into()
        });
    }
    let upstream = connect_to(&public, config.handshake).await?;
    line["upstream"] = json!(upstream.peer_addr().ok());
    Ok((upstream, head, credential))
}

async fn connect_to(addrs: &[SocketAddrV4], timeout: Duration) -> Result<TcpStream, String> {
    let mut last = String::from("no address");
    for addr in addrs {
        match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = format!("connecting to {addr}: {e}"),
            Err(_) => last = format!("connecting to {addr}: timed out"),
        }
    }
    Err(last)
}

/// Reads from the client until `parse` finds the name it asks for, and
/// returns it, normalized, with the bytes read.
async fn read_name(
    client: &mut TcpStream,
    parse: fn(&[u8]) -> Peek,
    timeout: Duration,
) -> Result<(String, Vec<u8>), String> {
    let read = async {
        let mut buf = Vec::with_capacity(2048);
        loop {
            match parse(&buf) {
                Peek::Name(n) => {
                    let n = rules::normalize(&n).ok_or("names an invalid name")?;
                    return Ok((n, buf));
                }
                Peek::NoName => return Err("names no destination".to_string()),
                Peek::Invalid => return Err("not the protocol for this port".to_string()),
                Peek::Incomplete if buf.len() >= PEEK_LIMIT => {
                    return Err("too long before naming a destination".to_string());
                }
                Peek::Incomplete => {}
            }
            let mut chunk = [0; 4096];
            let n = client
                .read(&mut chunk)
                .await
                .map_err(|e| format!("reading: {e}"))?;
            if n == 0 {
                return Err("closed before naming a destination".to_string());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    };
    tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| "didn't name a destination in time".to_string())?
}

/// Copies both ways until both sides are done or nothing moves for `idle`.
/// Returns the bytes sent each way, and why it ended.
async fn pipe(
    client: TcpStream,
    upstream: TcpStream,
    head: &[u8],
    idle: Duration,
) -> (u64, u64, &'static str) {
    let started = Instant::now();
    let last = AtomicU64::new(0);
    let (up, down) = (AtomicU64::new(head.len() as u64), AtomicU64::new(0));
    let (client_read, mut client_write) = client.into_split();
    let (upstream_read, mut upstream_write) = upstream.into_split();
    if upstream_write.write_all(head).await.is_err() {
        return (0, 0, "upstream closed");
    }
    let copy = async {
        tokio::join!(
            copy(client_read, &mut upstream_write, &up, &last, started),
            copy(upstream_read, &mut client_write, &down, &last, started),
        )
    };
    let watchdog = async {
        loop {
            let quiet = started
                .elapsed()
                .saturating_sub(Duration::from_millis(last.load(Ordering::Relaxed)));
            if quiet >= idle {
                return;
            }
            tokio::time::sleep(idle - quiet).await;
        }
    };
    let why = tokio::select! {
        _ = copy => "done",
        _ = watchdog => "idle",
    };
    (up.into_inner(), down.into_inner(), why)
}

async fn copy(
    mut from: tokio::net::tcp::OwnedReadHalf,
    to: &mut tokio::net::tcp::OwnedWriteHalf,
    bytes: &AtomicU64,
    last: &AtomicU64,
    started: Instant,
) {
    let mut buf = vec![0; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        last.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        if to.write_all(&buf[..n]).await.is_err() {
            break;
        }
        bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
    let _ = to.shutdown().await;
}

async fn answer_dns(
    config: Arc<Config>,
    vm: Arc<Vm>,
    socket: UdpSocket,
    mut stopped: watch::Receiver<()>,
) {
    let mut buf = [0; 1500];
    loop {
        let (n, peer) = tokio::select! {
            _ = stopped.changed() => return,
            received = socket.recv_from(&mut buf) => match received {
                Ok(r) => r,
                Err(_) => continue,
            },
        };
        if peer.ip() != vm.guest() {
            continue;
        }
        let response = match dns::parse(&buf[..n]) {
            Ok(query) => {
                let answer = dns_answer(&vm, &query);
                vm.log(
                    &config,
                    json!({"event": "dns", "name": query.name, "type": query.qtype,
                    "answer": match answer {
                        Answer::A(ip) => json!(ip),
                        Answer::Empty => json!("empty"),
                        Answer::NxDomain => json!("NXDOMAIN"),
                        Answer::ServFail => json!("SERVFAIL"),
                    }}),
                );
                dns::respond(&query, answer)
            }
            Err(Some(error)) => error,
            Err(None) => continue,
        };
        let _ = socket.send_to(&response, peer).await;
    }
}

fn dns_answer(vm: &Vm, query: &dns::Query) -> Answer {
    let Some(name) = rules::normalize(&query.name) else {
        return Answer::NxDomain;
    };
    if !vm.rules.read().unwrap().allows_name(&name) {
        return Answer::NxDomain;
    }
    if !query.wants_a() {
        return Answer::Empty;
    }
    match vm.names.lock().unwrap().address(&name) {
        Some(ip) => Answer::A(ip),
        None => Answer::ServFail,
    }
}

fn in_synthetic(ip: Ipv4Addr) -> bool {
    u32::from(ip) >> (32 - SYNTHETIC_PREFIX) == u32::from(SYNTHETIC) >> (32 - SYNTHETIC_PREFIX)
}

/// A VM's synthetic addresses: one per name it has looked up, kept for as
/// long as its policy runs.
#[derive(Default)]
struct Synthetic {
    by_name: HashMap<String, Ipv4Addr>,
    by_addr: HashMap<Ipv4Addr, String>,
}

impl Synthetic {
    /// The address standing for `name`, given out on first use. None when
    /// the range is used up.
    fn address(&mut self, name: &str) -> Option<Ipv4Addr> {
        if let Some(ip) = self.by_name.get(name) {
            return Some(*ip);
        }
        // From .0.1, skipping the range's first and last addresses.
        let size = 1u32 << (32 - SYNTHETIC_PREFIX);
        let next = self.by_name.len() as u32 + 1;
        if next >= size - 1 {
            return None;
        }
        let ip = Ipv4Addr::from(u32::from(SYNTHETIC) + next);
        self.by_name.insert(name.to_string(), ip);
        self.by_addr.insert(ip, name.to_string());
        Some(ip)
    }

    fn name(&self, ip: Ipv4Addr) -> Option<String> {
        self.by_addr.get(&ip).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_addresses_are_stable_and_in_range() {
        let mut s = Synthetic::default();
        let a = s.address("github.com").unwrap();
        let b = s.address("example.com").unwrap();
        assert_eq!(a, Ipv4Addr::new(198, 18, 0, 1));
        assert_eq!(b, Ipv4Addr::new(198, 18, 0, 2));
        assert_eq!(s.address("github.com"), Some(a));
        assert_eq!(s.name(b).as_deref(), Some("example.com"));
        assert_eq!(s.name(Ipv4Addr::new(198, 18, 0, 3)), None);
        assert!(in_synthetic(a));
        assert!(in_synthetic(Ipv4Addr::new(198, 19, 255, 254)));
        assert!(!in_synthetic(Ipv4Addr::new(198, 20, 0, 1)));
        assert!(!in_synthetic(Ipv4Addr::new(1, 1, 1, 1)));
    }

    fn vm(rules: &[&str]) -> Vm {
        Vm {
            id: "3f2a9c1b".into(),
            addr: Ipv4Addr::new(10, 200, 0, 5),
            rules: RwLock::new(Rules(rules.iter().map(|r| r.parse().unwrap()).collect())),
            names: Mutex::new(Synthetic::default()),
            slots: Arc::new(Semaphore::new(1)),
        }
    }

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn dns_answers_follow_the_rules() {
        let vm = vm(&["github.com:22", "*.githubusercontent.com"]);
        let answer = |name: &str, qtype| dns_answer(&vm, &dns::parse(&query(name, qtype)).unwrap());
        assert_eq!(
            answer("GitHub.com", dns::TYPE_A),
            Answer::A(Ipv4Addr::new(198, 18, 0, 1))
        );
        assert_eq!(answer("github.com", 28), Answer::Empty, "AAAA");
        assert_eq!(answer("github.com", 65), Answer::Empty, "HTTPS");
        assert_eq!(
            answer("raw.githubusercontent.com", dns::TYPE_A),
            Answer::A(Ipv4Addr::new(198, 18, 0, 2))
        );
        assert_eq!(answer("example.com", dns::TYPE_A), Answer::NxDomain);
        assert_eq!(
            answer("githubusercontent.com", dns::TYPE_A),
            Answer::NxDomain
        );
        assert_eq!(vm.guest(), Ipv4Addr::new(10, 200, 0, 6));
    }
}
