//! jete: boot, use and remove jailed Firecracker VMs.
//!
//! Runs as root (the jailer needs it). Every command works directly on the
//! host's state in /var/lib/vms; there is no daemon yet.

use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jete::agent::{self, AGENT_PORT};
use jete::net::{Cidr, NetSpec};
use jete::vm::{self, Host, Spec, Vm};
use jete_protocol::{Exec, ExitStatus, WindowSize};
use rustix::termios;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "jete",
    bin_name = "jete",
    version,
    about = "Fast Firecracker VM manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Boot a VM and wait until it is ready. Prints its id.
    Run {
        /// Guest kernel [default: /var/lib/vms/images/kernels/vmlinux]
        #[arg(long)]
        kernel: Option<PathBuf>,
        /// Root filesystem image [default: /var/lib/vms/images/rootfs/ubuntu-26.04.ext4]
        #[arg(long)]
        rootfs: Option<PathBuf>,
        /// Virtual CPUs
        #[arg(long, default_value_t = 2)]
        vcpus: u8,
        /// Guest memory in MiB
        #[arg(long, default_value_t = 512)]
        mem: u32,
        /// Give the VM a network that may reach this IPv4 address or CIDR
        /// block, any port. Repeatable. Without --allow or --host-port, the
        /// VM has no network at all.
        #[arg(long, value_name = "ADDRESS")]
        allow: Vec<Cidr>,
        /// Give the VM a network that may reach this TCP or UDP port on the
        /// host side of its link. Repeatable.
        #[arg(long, value_name = "PORT")]
        host_port: Vec<u16>,
        /// The guest's DNS resolver; must be reachable through --allow or
        /// --host-port
        #[arg(long, value_name = "ADDRESS")]
        dns: Option<Ipv4Addr>,
        /// Print the VM's record as JSON
        #[arg(long)]
        json: bool,
    },
    /// List VMs.
    Ls {
        /// Print the records as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run a command in a VM, passing stdin, stdout, stderr and the exit code
    /// through.
    Exec {
        id: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Open an interactive shell in a VM.
    Shell { id: String },
    /// Stop VMs and delete them, with everything they used.
    Rm {
        #[arg(required = true)]
        ids: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if !rustix::process::geteuid().is_root() {
        eprintln!("jete: must run as root (the jailer needs it); try sudo");
        return ExitCode::from(1);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("cannot start the async runtime");
    match runtime.block_on(run(cli.command)) {
        // Exit right away: a read of stdin may still be blocked on a thread
        // that would otherwise hold the process open.
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("jete: {e}");
            ExitCode::from(1)
        }
    }
}

async fn run(command: Command) -> io::Result<i32> {
    let host = Host::default();
    match command {
        Command::Run {
            kernel,
            rootfs,
            vcpus,
            mem,
            allow,
            host_port,
            dns,
            json,
        } => {
            let network = if allow.is_empty() && host_port.is_empty() {
                if dns.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--dns needs a network: add --allow or --host-port",
                    ));
                }
                None
            } else {
                Some(NetSpec {
                    allow,
                    host_ports: host_port,
                    dns,
                })
            };
            let defaults = Spec::default();
            let spec = Spec {
                kernel: kernel.unwrap_or(defaults.kernel),
                rootfs: rootfs.unwrap_or(defaults.rootfs),
                vcpus,
                mem_mib: mem,
                network,
            };
            let vm = Vm::start(&host, &spec).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&vm).map_err(io::Error::other)?
                );
            } else {
                println!("{}", vm.id);
                eprintln!("ready in {} ms", vm.ready_ms.unwrap_or_default());
            }
            Ok(0)
        }
        Command::Ls { json } => {
            let vms = vm::list(&host)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&vms).map_err(io::Error::other)?
                );
                return Ok(0);
            }
            println!(
                "{:<10} {:<8} {:>5} {:>8} {:>9}  ADDRESS",
                "ID", "STATE", "VCPUS", "MEM MiB", "READY ms"
            );
            for vm in vms {
                let state = if vm.is_running() {
                    "running"
                } else {
                    "stopped"
                };
                let ready = vm.ready_ms.map_or("-".into(), |ms| ms.to_string());
                let address = vm
                    .network
                    .as_ref()
                    .map_or("-".into(), |net| net.guest.to_string());
                println!(
                    "{:<10} {:<8} {:>5} {:>8} {:>9}  {}",
                    vm.id, state, vm.vcpus, vm.mem_mib, ready, address
                );
            }
            Ok(0)
        }
        Command::Exec { id, command } => {
            let vm = running(&host, &id)?;
            let stream = agent::connect(&vm.vsock(), AGENT_PORT).await?;
            let exec = Exec {
                argv: command,
                env: Vec::new(),
                cwd: None,
                tty: None,
            };
            let status = agent::exec(
                stream,
                exec,
                tokio::io::stdin(),
                tokio::io::stdout(),
                tokio::io::stderr(),
                None,
            )
            .await?;
            Ok(exit_code(status))
        }
        Command::Shell { id } => {
            let vm = running(&host, &id)?;
            shell(&vm).await
        }
        Command::Rm { ids } => {
            let mut failed = false;
            for id in ids {
                match Vm::load(&host, &id).and_then(|vm| vm.remove()) {
                    Ok(()) => println!("{id}"),
                    Err(e) => {
                        eprintln!("jete: {id}: {e}");
                        failed = true;
                    }
                }
            }
            Ok(i32::from(failed))
        }
    }
}

fn running(host: &Host, id: &str) -> io::Result<Vm> {
    let vm = Vm::load(host, id)?;
    if !vm.is_running() {
        return Err(io::Error::other(format!("VM {id} is not running")));
    }
    Ok(vm)
}

/// The process's exit code, or 128 plus the signal that killed it, as
/// shells report it.
fn exit_code(status: ExitStatus) -> i32 {
    match (status.code, status.signal) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => 1,
    }
}

async fn shell(vm: &Vm) -> io::Result<i32> {
    let stdin = std::io::stdin();
    if !termios::isatty(&stdin) {
        return Err(io::Error::other(
            "shell needs a terminal; use exec for scripts",
        ));
    }
    let size = window_size()?;
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
    let exec = Exec {
        argv: vec!["/bin/bash".into(), "--login".into()],
        env: vec![("TERM".into(), term)],
        cwd: None,
        tty: Some(size),
    };

    let (resize_tx, resize_rx) = mpsc::channel(4);
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    tokio::spawn(async move {
        while winch.recv().await.is_some() {
            if let Ok(size) = window_size()
                && resize_tx.send(size).await.is_err()
            {
                break;
            }
        }
    });

    let stream = agent::connect(&vm.vsock(), AGENT_PORT).await?;
    let raw = RawMode::enter()?;
    let status = agent::exec(
        stream,
        exec,
        tokio::io::stdin(),
        tokio::io::stdout(),
        tokio::io::sink(),
        Some(resize_rx),
    )
    .await;
    drop(raw);
    Ok(exit_code(status?))
}

fn window_size() -> io::Result<WindowSize> {
    let size = termios::tcgetwinsize(std::io::stdout())?;
    // Some terminals (script(1), serial lines) report 0x0; programs then
    // misbehave, so fall back to the classic size.
    if size.ws_row == 0 || size.ws_col == 0 {
        return Ok(WindowSize { rows: 24, cols: 80 });
    }
    Ok(WindowSize {
        rows: size.ws_row,
        cols: size.ws_col,
    })
}

/// The local terminal in raw mode, so every key goes to the guest's
/// terminal unchanged. Restored on drop.
struct RawMode(termios::Termios);

impl RawMode {
    fn enter() -> io::Result<RawMode> {
        let stdin = std::io::stdin();
        let original = termios::tcgetattr(&stdin)?;
        let mut raw = original.clone();
        raw.make_raw();
        termios::tcsetattr(&stdin, termios::OptionalActions::Now, &raw)?;
        Ok(RawMode(original))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(std::io::stdin(), termios::OptionalActions::Now, &self.0);
        let _ = std::io::stdout().flush();
    }
}
