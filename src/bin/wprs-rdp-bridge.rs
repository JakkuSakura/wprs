use clap::Parser;

use wprs::prelude::*;
use wprs::protocols::wprs::Endpoint;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Security {
    None,
    Tls,
}

impl std::str::FromStr for Security {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "tls" => Ok(Self::Tls),
            other => bail!("invalid --security {other:?} (expected: none|tls)"),
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "wprs-rdp-bridge")]
struct Args {
    /// WPRS endpoint to connect to (e.g. unix:///tmp/wprs.sock, tcp://127.0.0.1:4567).
    #[arg(long, value_name = "ENDPOINT")]
    wprs_endpoint: Endpoint,

    /// Address to bind the RDP server listener.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:3389")]
    rdp_listen: std::net::SocketAddr,

    /// Transport security for the RDP listener.
    ///
    /// When using ssh port forwarding, prefer `none` and rely on the ssh tunnel.
    #[arg(long, value_name = "MODE", default_value = "none")]
    security: Security,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let sec = match args.security {
        Security::None => wprs::rdp::Security::None,
        Security::Tls => wprs::rdp::Security::Tls,
    };
    wprs::rdp::run_bridge(args.wprs_endpoint, args.rdp_listen, sec).location(loc!())
}
