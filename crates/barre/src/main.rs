//! barre: the egress gateway service, and the commands that manage its
//! policies. Policy commands run as root.

use std::io::{self, Read};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use barre::ca::{self, Ca};
use barre::credentials;
use barre::policy::{self, Policy};
use barre::rules::Rule;
use barre::serve::{self, Config};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "barre",
    bin_name = "barre",
    version,
    about = "Egress gateway for jete's VMs: allows each connection by name"
)]
struct Cli {
    /// Where policies are kept
    #[arg(long, global = true, default_value = policy::DIR)]
    policies: PathBuf,
    /// Where credentials are kept
    #[arg(long, global = true, default_value = credentials::DIR)]
    credentials: PathBuf,
    /// Where the certificate authority is kept
    #[arg(long, global = true, default_value = ca::DIR)]
    ca: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the gateway
    Serve {
        /// The port VMs' traffic is redirected to (jete run --egress-proxy)
        #[arg(long, default_value_t = 15001)]
        port: u16,
    },
    /// Let the VM behind ADDRESS reach these destinations, replacing what
    /// it had. A rule is NAME, *.NAME or ADDRESS[/PREFIX], then optionally
    /// :PORT[,PORT...]; port 443 by default. NAME@CREDENTIAL, on 443 only,
    /// has barre set that credential on its requests.
    Allow {
        /// The VM's host-side address
        address: Ipv4Addr,
        /// The VM's id, for the logs
        #[arg(long)]
        vm: String,
        #[arg(value_name = "RULE")]
        rules: Vec<Rule>,
    },
    /// Remove the policy and credentials for ADDRESS: the VM behind it
    /// reaches nothing
    Revoke { address: Ipv4Addr },
    /// Set credential NAME for a VM, from one `Header: value` line on
    /// standard input, replacing it; or remove it with --rm
    Credential {
        /// The VM's host-side address, or its id
        vm: String,
        name: String,
        #[arg(long)]
        rm: bool,
    },
    /// The certificate authority for names with credentials
    #[command(subcommand)]
    Ca(CaCommand),
    /// List policies
    Ls,
}

#[derive(Subcommand)]
enum CaCommand {
    /// Make it, unless there is one
    Init,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("barre: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> io::Result<()> {
    let dir = cli.policies;
    let creds = cli.credentials;
    match cli.command {
        Command::Serve { port } => {
            let ca = match Ca::load(&cli.ca) {
                Ok(ca) => Some(Arc::new(ca)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(io::Error::other(format!("{}: {e}", cli.ca.display()))),
            };
            let config = Config {
                policies: dir,
                credentials: creds,
                ca,
                ..Config::new(port)
            };
            tokio::select! {
                r = serve::serve(config) => r,
                _ = tokio::signal::ctrl_c() => Ok(()),
            }
        }
        Command::Allow { address, vm, rules } => {
            let policy = Policy::new(address, &vm, rules)?;
            policy::write(&dir, address, &policy)?;
            prune(&dir, &creds)
        }
        Command::Revoke { address } => {
            policy::remove(&dir, address)?;
            credentials::remove_all(&creds, address)?;
            prune(&dir, &creds)
        }
        Command::Credential { vm, name, rm } => {
            prune(&dir, &creds)?;
            let address = find(&dir, &vm)?;
            if rm {
                return credentials::remove(&creds, address, &name);
            }
            let mut line = Vec::new();
            io::stdin().take(16 * 1024 + 1).read_to_end(&mut line)?;
            credentials::write(&creds, address, &name, &line)
        }
        Command::Ca(CaCommand::Init) => {
            if ca::init(&cli.ca)? {
                eprintln!("barre: made {}", cli.ca.join("ca.crt").display());
            }
            Ok(())
        }
        Command::Ls => {
            prune(&dir, &creds)?;
            println!("{:<15}  {:<8}  RULES", "ADDRESS", "VM");
            for (addr, policy) in policy::read_all(&dir)? {
                match policy {
                    Ok(p) => {
                        let rules: Vec<String> = p.rules.iter().map(Rule::to_string).collect();
                        println!("{addr:<15}  {:<8}  {}", p.vm, rules.join(" "));
                    }
                    Err(e) => println!("{addr:<15}  {:<8}  unreadable: {e}", "-"),
                }
            }
            Ok(())
        }
    }
}

/// Removes stale policies, and the credentials of addresses with no policy.
fn prune(dir: &Path, creds: &Path) -> io::Result<()> {
    policy::prune(dir)?;
    let live: Vec<Ipv4Addr> = policy::read_all(dir)?
        .into_iter()
        .filter(|(_, p)| p.is_ok())
        .map(|(a, _)| a)
        .collect();
    credentials::prune(creds, &live)
}

/// The address of the VM `vm` names: an address with a policy, or the id
/// of a VM with one.
fn find(dir: &Path, vm: &str) -> io::Result<Ipv4Addr> {
    let all = policy::read_all(dir)?;
    let found = if let Ok(addr) = vm.parse::<Ipv4Addr>() {
        all.iter().find(|(a, p)| *a == addr && p.is_ok())
    } else {
        all.iter()
            .find(|(_, p)| p.as_ref().is_ok_and(|p| p.vm == vm))
    };
    found.map(|(a, _)| *a).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no policy for {vm}: barre allow it first"),
        )
    })
}
