//! jete-agent: started by systemd at boot, listens on vsock for the host.
//! Logs go to stderr, which systemd sends to the journal.

use jete_protocol::AGENT_PORT;
use tokio_vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VsockAddr, VsockListener};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let listener = match VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, AGENT_PORT)) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("jete-agent: cannot listen on vsock port {AGENT_PORT}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "jete-agent {} listening on vsock port {AGENT_PORT}",
        env!("CARGO_PKG_VERSION")
    );
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!("jete-agent: accept failed: {e}");
                continue;
            }
        };
        // Only the host may connect. vsock loopback would otherwise let any
        // process in the guest run commands as root.
        if peer.cid() != VMADDR_CID_HOST {
            eprintln!("jete-agent: refused connection from cid {}", peer.cid());
            continue;
        }
        tokio::spawn(async move {
            if let Err(e) = jete_agent::serve(stream).await {
                eprintln!("jete-agent: session failed: {e}");
            }
        });
    }
}
