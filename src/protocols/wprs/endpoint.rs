// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use anyhow::ensure;

use crate::prelude::*;

#[derive(Debug, Clone, Eq, PartialEq, serde_derive::Serialize, serde_derive::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Endpoint {
    /// Unix domain socket path.
    ///
    /// Preferred for local connections on Unix platforms.
    Unix { path: PathBuf },
    /// TCP address.
    ///
    /// Allowed, but non-loopback addresses should be treated as unsafe unless you
    /// add authentication + encryption at a higher layer.
    Tcp { addr: SocketAddr },

    /// Connect via an SSH local-forward tunnel.
    ///
    /// The client spawns `ssh` to forward a local Unix socket or TCP port to a
    /// remote Unix socket or TCP port, then connects to the local forwarded
    /// endpoint.
    ///
    /// This is a convenience wrapper; authentication/encryption are delegated
    /// to OpenSSH.
    Ssh {
        destination: SshDestination,
        remote: Box<Endpoint>,
        #[serde(default)]
        local: Option<Box<Endpoint>>,
        #[serde(default)]
        ssh_args: Vec<String>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, serde_derive::Serialize, serde_derive::Deserialize)]
pub struct SshDestination {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Unix { path } => write!(f, "unix://{}", path.display()),
            Endpoint::Tcp { addr } => write!(f, "tcp://{addr}"),
            Endpoint::Ssh {
                destination,
                remote,
                local,
                ssh_args,
            } => {
                write!(
                    f,
                    "ssh://{}?remote={remote}",
                    format_ssh_destination(destination)
                )?;
                if let Some(local) = local {
                    write!(f, "&local={local}")?;
                }
                for a in ssh_args {
                    write!(f, "&ssh-arg={a}")?;
                }
                Ok(())
            },
        }
    }
}

fn format_ssh_destination(destination: &SshDestination) -> String {
    let host = if destination.host.contains(':') {
        format!("[{}]", destination.host)
    } else {
        destination.host.clone()
    };

    let mut out = String::new();
    if let Some(user) = &destination.user {
        out.push_str(user);
        out.push('@');
    }
    out.push_str(&host);
    if let Some(port) = destination.port {
        out.push(':');
        out.push_str(&port.to_string());
    }
    out
}

impl Endpoint {
    pub fn warn_if_non_loopback(&self, kind: &str) {
        if let Endpoint::Tcp { addr } = self {
            if !addr.ip().is_loopback() {
                warn!(
                    "{kind} is bound to {addr:?} (non-loopback). This is not recommended without authentication/encryption. Prefer localhost (127.0.0.1/::1)."
                );
            }
        }
    }
}

/// A client-side transport guard that keeps any background forwarding (e.g.
/// `ssh -L ...`) alive for as long as it is held.
pub struct ClientTransportGuard(pub(crate) TransportGuard);

impl ClientTransportGuard {
    pub(crate) fn into_inner(self) -> TransportGuard {
        self.0
    }
}

/// Resolve an [`Endpoint`] to a concrete local endpoint suitable for a client
/// to connect to.
///
/// For non-SSH endpoints this is a no-op.
///
/// For `ssh://...` endpoints this spawns `ssh` to create a local-forward tunnel
/// and returns the chosen local endpoint plus a guard that keeps the tunnel
/// alive.
pub fn setup_client_transport(
    endpoint: Endpoint,
) -> Result<(Endpoint, Option<ClientTransportGuard>)> {
    match endpoint {
        Endpoint::Ssh {
            destination,
            remote,
            local,
            ssh_args,
        } => {
            let (local_endpoint, guard) =
                setup_ssh_forwarding(destination, *remote, local.map(|b| *b), ssh_args)
                    .location(loc!())?;
            Ok((local_endpoint, Some(ClientTransportGuard(guard))))
        },
        other => Ok((other, None)),
    }
}

impl std::str::FromStr for Endpoint {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Note: check the URI form first, otherwise `strip_prefix("tcp:")` would
        // accept `tcp://...` and leave a leading `//`.
        if let Some(rest) = s.strip_prefix("tcp://") {
            let addr: SocketAddr = rest
                .parse()
                .map_err(|e| anyhow!("invalid tcp endpoint {rest:?}: {e}"))?;
            return Ok(Self::Tcp { addr });
        }

        if let Some(rest) = s.strip_prefix("tcp:") {
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            let addr: SocketAddr = rest
                .parse()
                .map_err(|e| anyhow!("invalid tcp endpoint {rest:?}: {e}"))?;
            return Ok(Self::Tcp { addr });
        }

        if let Some(rest) = s.strip_prefix("unix:") {
            return Ok(Self::Unix {
                path: PathBuf::from(rest),
            });
        }

        if let Some(rest) = s.strip_prefix("unix://") {
            #[cfg(unix)]
            {
                let path = rest.strip_prefix('/').unwrap_or(rest);
                // Preserve absolute paths for the common `unix:///abs/path` form.
                let path = if rest.starts_with('/') {
                    PathBuf::from(rest)
                } else {
                    PathBuf::from(path)
                };
                return Ok(Self::Unix { path });
            }

            #[cfg(not(unix))]
            {
                let _ = rest;
                bail!("unix endpoint is not supported on this platform")
            }
        }

        if let Some(rest) = s.strip_prefix("ssh://") {
            return parse_ssh_endpoint(rest).location(loc!());
        }

        #[cfg(unix)]
        return Ok(Self::Unix {
            path: PathBuf::from(s),
        });

        #[cfg(not(unix))]
        bail!("invalid endpoint {s:?} (expected: tcp:IP:PORT)")
    }
}

fn parse_ssh_endpoint(rest: &str) -> Result<Endpoint> {
    let (authority_and_path, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };

    let (authority, remote_from_path) = match authority_and_path.split_once('/') {
        Some((a, p)) if !p.is_empty() => (a, Some(p)),
        _ => (authority_and_path, None),
    };

    let destination = parse_ssh_destination(authority).location(loc!())?;

    let mut remote_str: Option<&str> = remote_from_path;
    let mut local_str: Option<&str> = None;
    let mut ssh_args: Vec<String> = Vec::new();

    if let Some(query) = query {
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair
                .split_once('=')
                .ok_or_else(|| anyhow!("invalid ssh endpoint query item {pair:?} (expected k=v)"))
                .location(loc!())?;

            match k {
                "remote" => remote_str = Some(v),
                "local" => local_str = Some(v),
                "ssh-arg" => ssh_args.push(v.to_string()),
                other => bail!(
                    "unknown ssh endpoint query key {other:?} (expected: remote|local|ssh-arg)"
                ),
            }
        }
    }

    let remote_str = remote_str
        .ok_or_else(|| anyhow!("ssh endpoint requires remote=<endpoint> or ssh://HOST/<endpoint>"))
        .location(loc!())?;
    let remote: Endpoint = remote_str.parse().location(loc!())?;

    let local: Option<Endpoint> = match local_str {
        Some(s) => Some(s.parse().location(loc!())?),
        None => None,
    };

    Ok(Endpoint::Ssh {
        destination,
        remote: Box::new(remote),
        local: local.map(Box::new),
        ssh_args,
    })
}

fn parse_ssh_destination(authority: &str) -> Result<SshDestination> {
    // Supported forms:
    // - host
    // - user@host
    // - host:22
    // - user@host:22
    // - [::1]:22
    // - user@[::1]:22
    let (user, hostport) = match authority.split_once('@') {
        Some((u, hp)) => (Some(u.to_string()), hp),
        None => (None, authority),
    };

    let (host, port) = if let Some(hp) = hostport.strip_prefix('[') {
        let (host, rest) = hp
            .split_once(']')
            .ok_or_else(|| anyhow!("invalid ssh host {hostport:?} (missing ']')"))?;
        let port = rest
            .strip_prefix(':')
            .map(|p| p.parse::<u16>())
            .transpose()?;
        (host.to_string(), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                (h.to_string(), Some(p.parse::<u16>()?))
            },
            _ => (hostport.to_string(), None),
        }
    };

    ensure!(!host.is_empty(), "ssh destination host is empty");
    Ok(SshDestination { user, host, port })
}

#[allow(dead_code)]
pub(crate) enum TransportGuard {
    Ssh(SshTunnel),
}

pub(crate) struct SshTunnel {
    child: std::process::Child,
    #[cfg(unix)]
    local_unix_socket: Option<PathBuf>,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();

        #[cfg(unix)]
        if let Some(path) = &self.local_unix_socket {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn setup_ssh_forwarding(
    destination: SshDestination,
    remote: Endpoint,
    local: Option<Endpoint>,
    ssh_args: Vec<String>,
) -> Result<(Endpoint, TransportGuard)> {
    use std::process::Stdio;

    let (local, local_unix_socket_to_cleanup) =
        choose_local_forward_endpoint(&remote, local).location(loc!())?;

    let mut cmd = std::process::Command::new("ssh");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    cmd.arg("-N");
    cmd.arg("-o").arg("ExitOnForwardFailure=yes");

    if let Some(port) = destination.port {
        cmd.arg("-p").arg(port.to_string());
    }

    match (&local, &remote) {
        (Endpoint::Tcp { addr: l }, Endpoint::Tcp { addr: r }) => {
            // Local binds to loopback to avoid exposing an unauthenticated TCP port.
            ensure!(
                l.ip().is_loopback(),
                "ssh local tcp endpoint must be loopback"
            );
            cmd.arg("-L")
                .arg(format!("{}:{}:{}:{}", l.ip(), l.port(), r.ip(), r.port()));
        },
        #[cfg(unix)]
        (Endpoint::Unix { path: l }, Endpoint::Unix { path: r }) => {
            cmd.arg("-o").arg("StreamLocalBindUnlink=yes");
            cmd.arg("-L")
                .arg(format!("{}:{}", l.display(), r.display()));
        },
        #[cfg(not(unix))]
        (Endpoint::Unix { .. }, _) | (_, Endpoint::Unix { .. }) => {
            bail!("unix socket forwarding over ssh is not supported on this platform")
        },
        _ => bail!(
            "ssh forwarding requires local and remote endpoints to have the same type (tcp or unix)"
        ),
    }

    for a in ssh_args {
        cmd.arg(a);
    }

    let dest = match destination.user {
        Some(user) => format!("{user}@{}", destination.host),
        None => destination.host,
    };
    cmd.arg(dest);

    info!("starting ssh tunnel: {cmd:?}");
    let mut child = cmd.spawn().location(loc!())?;

    if let Err(err) = wait_for_local_forward_ready(&local, Duration::from_secs(5)) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err).location(loc!());
    }

    let tunnel = SshTunnel {
        child,
        #[cfg(unix)]
        local_unix_socket: local_unix_socket_to_cleanup,
    };
    Ok((local, TransportGuard::Ssh(tunnel)))
}

fn choose_local_forward_endpoint(
    remote: &Endpoint,
    local: Option<Endpoint>,
) -> Result<(Endpoint, Option<PathBuf>)> {
    if let Some(local) = local {
        match &local {
            Endpoint::Tcp { addr } => {
                ensure!(
                    addr.ip().is_loopback(),
                    "ssh local tcp endpoint must be loopback"
                )
            },
            Endpoint::Unix { .. } => {
                #[cfg(not(unix))]
                bail!("unix endpoint is not supported on this platform")
            },
            Endpoint::Ssh { .. } => bail!("nested ssh endpoints are not supported"),
        }
        return Ok((local, None));
    }

    match remote {
        Endpoint::Tcp { .. } => {
            let port = allocate_ephemeral_loopback_port().location(loc!())?;
            Ok((
                Endpoint::Tcp {
                    addr: SocketAddr::from(([127, 0, 0, 1], port)),
                },
                None,
            ))
        },
        Endpoint::Unix { .. } => {
            #[cfg(unix)]
            {
                let path = default_temp_socket_path("wprs-ssh").location(loc!())?;
                Ok((Endpoint::Unix { path: path.clone() }, Some(path)))
            }

            #[cfg(not(unix))]
            bail!("unix endpoint is not supported on this platform")
        },
        Endpoint::Ssh { .. } => bail!("nested ssh endpoints are not supported"),
    }
}

fn allocate_ephemeral_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).location(loc!())?;
    Ok(listener.local_addr().location(loc!())?.port())
}

#[cfg(unix)]
fn default_temp_socket_path(prefix: &str) -> Result<PathBuf> {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(std::env::temp_dir().join(format!("{prefix}-{pid}-{ts}.sock")))
}

fn wait_for_local_forward_ready(endpoint: &Endpoint, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        let ready = match endpoint {
            Endpoint::Tcp { addr } => {
                TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_ok()
            },
            Endpoint::Unix { path } => {
                #[cfg(unix)]
                {
                    UnixStream::connect(path).is_ok()
                }

                #[cfg(not(unix))]
                {
                    let _ = path;
                    false
                }
            },
            Endpoint::Ssh { .. } => {
                bail!("nested ssh endpoints are not supported")
            },
        };

        if ready {
            return Ok(());
        }
        if Instant::now() - start > timeout {
            bail!("ssh local forward failed to open within timeout")
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn parse_tcp_uri_form() {
        let ep: Endpoint = "tcp://127.0.0.1:1234".parse().unwrap();
        assert_eq!(
            ep,
            Endpoint::Tcp {
                addr: SocketAddr::from(([127, 0, 0, 1], 1234)),
            }
        );
    }

    #[test]
    fn parse_unix_uri_form() {
        #[cfg(unix)]
        {
            let ep: Endpoint = "unix:///tmp/wprs.sock".parse().unwrap();
            assert_eq!(
                ep,
                Endpoint::Unix {
                    path: PathBuf::from("/tmp/wprs.sock"),
                }
            );
        }
    }

    #[test]
    fn parse_ssh_with_remote_query() {
        let ep: Endpoint =
            "ssh://me@example.com?remote=tcp://127.0.0.1:1&ssh-arg=-v"
                .parse()
                .unwrap();
        assert_eq!(
            ep,
            Endpoint::Ssh {
                destination: SshDestination {
                    user: Some("me".to_string()),
                    host: "example.com".to_string(),
                    port: None,
                },
                remote: Box::new(Endpoint::Tcp {
                    addr: SocketAddr::from(([127, 0, 0, 1], 1)),
                }),
                local: None,
                ssh_args: vec!["-v".to_string()],
            }
        );
    }

    #[test]
    fn parse_ssh_with_remote_path_and_port() {
        let ep: Endpoint = "ssh://me@example.com:2222/tcp://127.0.0.1:1".parse().unwrap();
        assert_eq!(
            ep,
            Endpoint::Ssh {
                destination: SshDestination {
                    user: Some("me".to_string()),
                    host: "example.com".to_string(),
                    port: Some(2222),
                },
                remote: Box::new(Endpoint::Tcp {
                    addr: SocketAddr::from(([127, 0, 0, 1], 1)),
                }),
                local: None,
                ssh_args: vec![],
            }
        );
    }

    #[test]
    fn setup_client_transport_passthrough_tcp() {
        let ep = Endpoint::Tcp {
            addr: SocketAddr::from(([127, 0, 0, 1], 1234)),
        };
        let (resolved, guard) = setup_client_transport(ep.clone()).unwrap();
        assert_eq!(resolved, ep);
        assert!(guard.is_none());
    }

    #[test]
    fn endpoint_display_tcp_uri() {
        let ep = Endpoint::Tcp {
            addr: SocketAddr::from(([127, 0, 0, 1], 1234)),
        };
        assert_eq!(ep.to_string(), "tcp://127.0.0.1:1234");
    }
}
