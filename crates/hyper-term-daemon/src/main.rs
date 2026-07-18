use std::path::PathBuf;

#[cfg(unix)]
use hyper_term_daemon::{DaemonState, run_unix_server};

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
            "hyperd\n\nUsage: hyperd [--state-dir PATH] [--socket PATH]\n\n\
             Defaults:\n  state dir  .hyper-term\n  socket     <state-dir>/hyperd.sock"
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
    run_unix_server(socket, state).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn run() -> Result<(), String> {
    Err("hyperd currently requires a Unix domain socket host".into())
}

#[derive(Default)]
struct Options {
    state_directory: Option<PathBuf>,
    socket: Option<PathBuf>,
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
                "-h" | "--help" => options.help = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(options)
    }
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
}
