//! barre: the egress gateway service, and the commands that manage its
//! policies. Policy commands run as root.

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// :PORT[,PORT...]; port 443 by default.
    Allow {
        /// The VM's host-side address
        address: Ipv4Addr,
        /// The VM's id, for the logs
        #[arg(long)]
        vm: String,
        #[arg(value_name = "RULE")]
        rules: Vec<Rule>,
    },
    /// Remove the policy for ADDRESS: the VM behind it reaches nothing
    Revoke { address: Ipv4Addr },
    /// List policies
    Ls,
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
    match cli.command {
        Command::Serve { port } => {
            let config = Config {
                policies: dir,
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
            policy::prune(&dir).map(drop)
        }
        Command::Revoke { address } => {
            policy::remove(&dir, address)?;
            policy::prune(&dir).map(drop)
        }
        Command::Ls => {
            policy::prune(&dir)?;
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
