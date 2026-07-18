use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use hyper_term_daemon::{
    DaemonState, TerminalGatewayConfig, run_unix_server, spawn_terminal_gateway, spawn_unix_server,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("hyperd: {error}");
        std::process::exit(1);
    }
}

#[cfg(unix)]
fn run() -> Result<(), String> {
    let options = Options::parse(std::env::args().skip(1))?;
    if options.help {
        println!(
            "hyperd\n\nUsage: hyperd [OPTIONS]\n\n\
             Options:\n  --state-dir PATH           Durable daemon state (default: .hyper-term)\n  \
             --socket PATH              Unix control socket (default: <state-dir>/hyperd.sock)\n  \
             --terminal-assets PATH     Enable the local terminal renderer gateway\n  \
             --terminal-http ADDRESS    Loopback gateway address (default: 127.0.0.1:47437)\n  \
             --terminal-token-file PATH File containing a 32+ byte gateway token (mode 0600)\n  \
             -h, --help                 Show this help"
        );
        return Ok(());
    }
    let state_directory = options
        .state_directory
        .unwrap_or_else(|| PathBuf::from(".hyper-term"));
    let socket = options
        .socket
        .unwrap_or_else(|| state_directory.join("hyperd.sock"));
    let state = DaemonState::open(&state_directory).map_err(|error| error.to_string())?;
    let Some(assets) = options.terminal_assets else {
        if options.terminal_http.is_some() || options.terminal_token_file.is_some() {
            return Err(
                "--terminal-http and --terminal-token-file require --terminal-assets".into(),
            );
        }
        return run_unix_server(socket, state).map_err(|error| error.to_string());
    };
    let token_file = options
        .terminal_token_file
        .ok_or_else(|| "--terminal-assets requires --terminal-token-file".to_owned())?;
    let token = read_terminal_token(&token_file)?;
    let bind = options
        .terminal_http
        .unwrap_or_else(|| "127.0.0.1:47437".parse().expect("valid default address"));
    let _control_server =
        spawn_unix_server(socket, state.clone()).map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("hyperd-async")
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let gateway = spawn_terminal_gateway(
            TerminalGatewayConfig {
                bind,
                assets,
                token,
                default_cwd: None,
            },
            state,
        )
        .await
        .map_err(|error| error.to_string())?;
        eprintln!(
            "hyperd: terminal gateway listening on http://{}",
            gateway.address()
        );
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| error.to_string())?;
        gateway.shutdown().await.map_err(|error| error.to_string())
    })
}

#[cfg(not(unix))]
fn run() -> Result<(), String> {
    Err("hyperd currently requires a Unix domain socket host".into())
}

#[derive(Default)]
struct Options {
    state_directory: Option<PathBuf>,
    socket: Option<PathBuf>,
    terminal_assets: Option<PathBuf>,
    terminal_http: Option<SocketAddr>,
    terminal_token_file: Option<PathBuf>,
    help: bool,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--state-dir" => {
                    options.state_directory = Some(PathBuf::from(
                        arguments
                            .next()
                            .ok_or_else(|| "--state-dir requires a path".to_owned())?,
                    ));
                }
                "--socket" => {
                    options.socket = Some(PathBuf::from(
                        arguments
                            .next()
                            .ok_or_else(|| "--socket requires a path".to_owned())?,
                    ));
                }
                "--terminal-assets" => {
                    options.terminal_assets =
                        Some(PathBuf::from(arguments.next().ok_or_else(|| {
                            "--terminal-assets requires a path".to_owned()
                        })?));
                }
                "--terminal-http" => {
                    let address = arguments
                        .next()
                        .ok_or_else(|| "--terminal-http requires an address".to_owned())?;
                    options.terminal_http = Some(
                        address
                            .parse()
                            .map_err(|_| format!("invalid --terminal-http address: {address}"))?,
                    );
                }
                "--terminal-token-file" => {
                    options.terminal_token_file =
                        Some(PathBuf::from(arguments.next().ok_or_else(|| {
                            "--terminal-token-file requires a path".to_owned()
                        })?));
                }
                "-h" | "--help" => options.help = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(options)
    }
}

#[cfg(unix)]
fn read_terminal_token(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot read terminal token metadata: {error}"))?;
    if !metadata.is_file() {
        return Err("terminal token path must be a regular file".into());
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("terminal token file must not be accessible by group or other users".into());
    }
    let token = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read terminal token: {error}"))?
        .trim()
        .to_owned();
    if token.len() < 32 {
        return Err("terminal token must contain at least 32 bytes".into());
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_follow_state_directory_for_default_socket() {
        let options = Options::parse(["--state-dir".into(), "/tmp/hyper-state".into()]).unwrap();
        assert_eq!(
            options.state_directory,
            Some(PathBuf::from("/tmp/hyper-state"))
        );
        assert_eq!(options.socket, None);
    }

    #[test]
    fn options_reject_unknown_arguments() {
        assert!(Options::parse(["--serve".into()]).is_err());
    }

    #[test]
    fn options_parse_terminal_gateway_without_accepting_a_public_default() {
        let options = Options::parse([
            "--terminal-assets".into(),
            "dist/terminal".into(),
            "--terminal-token-file".into(),
            "/tmp/hyper-term-token".into(),
        ])
        .expect("options");

        assert_eq!(
            options.terminal_assets,
            Some(PathBuf::from("dist/terminal"))
        );
        assert_eq!(
            options.terminal_token_file,
            Some(PathBuf::from("/tmp/hyper-term-token"))
        );
        assert_eq!(options.terminal_http, None);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_token_file_must_be_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let token = directory.path().join("token");
        std::fs::write(&token, "0123456789abcdef0123456789abcdef\n").expect("write token");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644))
            .expect("public permissions");
        assert!(read_terminal_token(&token).is_err());

        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))
            .expect("private permissions");
        assert_eq!(
            read_terminal_token(&token).expect("private token"),
            "0123456789abcdef0123456789abcdef"
        );
    }
}
