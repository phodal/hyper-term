#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

#[cfg(unix)]
use hyper_term_core::JournalError;
#[cfg(unix)]
use hyper_term_daemon::{DaemonError, DaemonState};

#[test]
#[cfg(unix)]
fn state_root_is_private_and_has_one_daemon_writer() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let state_root = temporary.path().join("state");
    std::fs::create_dir(&state_root).expect("state root");
    std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o755))
        .expect("public fixture permissions");

    let first = DaemonState::open(&state_root).expect("first daemon");
    let canonical = state_root.canonicalize().expect("canonical state root");
    assert_eq!(
        std::fs::metadata(&canonical)
            .expect("state root metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(canonical.join("daemon.lock"))
            .expect("lock metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(canonical.join("events.jsonl"))
            .expect("journal metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let retained = first.clone();
    assert!(matches!(
        DaemonState::open(&state_root),
        Err(DaemonError::StateDirectoryInUse(path)) if path == canonical
    ));
    drop(first);
    assert!(matches!(
        DaemonState::open(&state_root),
        Err(DaemonError::StateDirectoryInUse(path)) if path == canonical
    ));

    drop(retained);
    DaemonState::open(&state_root).expect("lock released after the last clone");
}

#[test]
#[cfg(unix)]
fn daemon_rejects_a_redirected_event_journal() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let state_root = temporary.path().join("state");
    std::fs::create_dir(&state_root).expect("state root");
    let outside = temporary.path().join("outside.jsonl");
    std::fs::File::create(&outside).expect("outside journal");
    let journal = state_root.join("events.jsonl");
    symlink(&outside, &journal).expect("journal symlink");
    let canonical_journal = state_root
        .canonicalize()
        .expect("canonical state root")
        .join("events.jsonl");

    let error = match DaemonState::open(&state_root) {
        Ok(_) => panic!("redirected journal was accepted"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, DaemonError::Journal(JournalError::UnsafePath(path)) if path == &canonical_journal),
        "unexpected startup error: {error:?}"
    );
}
