use std::{
    io::{self, Read},
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::mpsc::Sender,
    thread,
};

use crate::metadata::{TunnelDefinition, TunnelKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelEvent {
    Stderr { id: uuid::Uuid, message: String },
}

pub fn spawn_forward(
    tunnel: &TunnelDefinition,
    events: Sender<TunnelEvent>,
    auth: &crate::credentials::HostAuth,
) -> io::Result<(Child, Option<crate::authentication::Status>)> {
    check_local_bind(tunnel)?;
    let (option, forward) = forward_spec(tunnel)?;
    let mut command = Command::new("ssh");
    command
        .arg("-N")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg(format!(
            "ConnectTimeout={}",
            tunnel.connect_timeout_secs.max(1)
        ))
        .arg("-o")
        .arg(format!(
            "ServerAliveInterval={}",
            tunnel.keepalive_interval_secs
        ))
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg(option)
        .arg(forward)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let trace = crate::credentials::configure_host_auth(&mut command, auth, &tunnel.host_alias)?;
    let authentication = trace.as_ref().map(crate::authentication::Trace::status);
    command.arg(&tunnel.host_alias);
    let mut child = command.spawn()?;
    if let Some(mut stderr) = child.stderr.take() {
        let id = tunnel.id;
        if let Err(error) = thread::Builder::new()
            .name(format!("lazyterm-tunnel-{id}"))
            .spawn(move || {
                let mut output = String::new();
                let _ = crate::authentication::Reader::new(&mut stderr, trace)
                    .read_to_string(&mut output);
                let message = crate::diagnostics::redact_sensitive_text(output.trim());
                if !message.is_empty() {
                    let _ = events.send(TunnelEvent::Stderr { id, message });
                }
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(format!(
                "Could not monitor tunnel stderr: {error}"
            )));
        }
    }
    Ok((child, authentication))
}

pub fn check_local_bind(tunnel: &TunnelDefinition) -> io::Result<()> {
    if tunnel.kind == TunnelKind::Remote {
        return Ok(());
    }
    TcpListener::bind((&*tunnel.bind_address, tunnel.local_port))
        .map(drop)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "local bind {}:{} is unavailable: {error}",
                    tunnel.bind_address, tunnel.local_port
                ),
            )
        })
}

fn forward_spec(tunnel: &TunnelDefinition) -> io::Result<(&'static str, String)> {
    match tunnel.kind {
        TunnelKind::Local | TunnelKind::Remote => {
            let remote_host = tunnel.remote_host.as_deref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "destination host is missing")
            })?;
            let remote_port = tunnel.remote_port.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "destination port is missing")
            })?;
            Ok((
                if tunnel.kind == TunnelKind::Local {
                    "-L"
                } else {
                    "-R"
                },
                format!(
                    "{}:{}:{}:{}",
                    tunnel.bind_address, tunnel.local_port, remote_host, remote_port
                ),
            ))
        }
        TunnelKind::Dynamic => Ok((
            "-D",
            format!("{}:{}", tunnel.bind_address, tunnel.local_port),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_specs_use_expected_ssh_arguments() {
        let tunnel = TunnelDefinition {
            id: uuid::Uuid::from_u128(4),
            label: "database".into(),
            host_alias: "web".into(),
            custom_host: false,
            kind: crate::metadata::TunnelKind::Local,
            bind_address: "127.0.0.1".into(),
            local_port: 15432,
            remote_host: Some("db.internal".into()),
            remote_port: Some(5432),
            lifecycle: crate::metadata::TunnelLifecycle::App,
            auto_reconnect: false,
            connect_timeout_secs: 10,
            keepalive_interval_secs: 30,
        };
        assert_eq!(
            forward_spec(&tunnel).unwrap(),
            ("-L", "127.0.0.1:15432:db.internal:5432".into())
        );
        let mut remote = tunnel.clone();
        remote.kind = TunnelKind::Remote;
        assert_eq!(
            forward_spec(&remote).unwrap(),
            ("-R", "127.0.0.1:15432:db.internal:5432".into())
        );
        let mut dynamic = tunnel;
        dynamic.kind = TunnelKind::Dynamic;
        dynamic.remote_host = None;
        dynamic.remote_port = None;
        assert_eq!(
            forward_spec(&dynamic).unwrap(),
            ("-D", "127.0.0.1:15432".into())
        );
    }

    #[test]
    fn local_bind_conflicts_are_reported_before_startup() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let tunnel = TunnelDefinition {
            id: uuid::Uuid::from_u128(9),
            label: "conflict".into(),
            host_alias: "web".into(),
            custom_host: false,
            kind: TunnelKind::Local,
            bind_address: "127.0.0.1".into(),
            local_port: port,
            remote_host: Some("db.internal".into()),
            remote_port: Some(5432),
            lifecycle: crate::metadata::TunnelLifecycle::App,
            auto_reconnect: false,
            connect_timeout_secs: 10,
            keepalive_interval_secs: 30,
        };
        let error = check_local_bind(&tunnel).unwrap_err().to_string();
        assert!(error.contains("local bind 127.0.0.1:"));
    }
}
