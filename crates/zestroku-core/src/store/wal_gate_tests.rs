//! Deterministic controls for the restore sidecar gate. These need no
//! driver: the gate is filesystem policy over a proved-complete checkpoint.

use super::{WAL_HEADER_BYTES, checkpoint_complete, settle_sidecars, sidecar};

fn live_in(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("actions.db")
}

#[test]
fn checkpoint_status_requires_not_busy_and_a_reset_log() {
    // TRUNCATE completed, or the non-WAL sentinel: quiescent.
    assert!(checkpoint_complete(0, 0, 0));
    assert!(checkpoint_complete(0, -1, -1));
    // Busy (active writer / held fence) is Ok-with-busy=1 from the driver.
    assert!(!checkpoint_complete(1, 0, 0));
    assert!(!checkpoint_complete(1, 5, 0));
    // Not busy but frames remain un-backfilled: not complete.
    assert!(!checkpoint_complete(0, 5, 3));
    assert!(!checkpoint_complete(0, 5, 5));
    assert!(!checkpoint_complete(0, 0, 1));
}

#[test]
fn header_only_wal_and_shm_are_removed_after_a_proved_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let live = live_in(&dir);
    std::fs::write(&live, b"db").unwrap();
    std::fs::write(sidecar(&live, "-wal"), vec![0u8; WAL_HEADER_BYTES as usize]).unwrap();
    std::fs::write(sidecar(&live, "-shm"), vec![0u8; 32768]).unwrap();

    settle_sidecars(&live).unwrap();

    assert!(!sidecar(&live, "-wal").exists());
    assert!(!sidecar(&live, "-shm").exists());
    assert!(live.exists(), "settling never touches the database file");
}

#[test]
fn frames_beyond_the_header_refuse_and_are_never_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let live = live_in(&dir);
    std::fs::write(&live, b"db").unwrap();
    let wal = sidecar(&live, "-wal");
    let frames = vec![7u8; WAL_HEADER_BYTES as usize + 1];
    std::fs::write(&wal, &frames).unwrap();

    let err = settle_sidecars(&live).unwrap_err();
    assert!(err.to_string().contains("un-checkpointed"), "{err}");

    // The WAL bytes survive intact, and nothing else was removed first.
    assert_eq!(std::fs::read(&wal).unwrap(), frames);
}

#[test]
fn an_absent_wal_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let live = live_in(&dir);
    std::fs::write(&live, b"db").unwrap();
    settle_sidecars(&live).unwrap();
}

#[cfg(unix)]
#[test]
fn an_uninspectable_wal_refuses_instead_of_reading_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let live = live_in(&dir);
    std::fs::write(&live, b"db").unwrap();
    let wal = sidecar(&live, "-wal");
    // A symlink loop: stat fails with ELOOP, which is not NotFound.
    std::os::unix::fs::symlink(&wal, &wal).unwrap();

    let err = settle_sidecars(&live).unwrap_err();
    assert!(err.to_string().contains("cannot stat live WAL"), "{err}");

    // Fail closed: the unreadable sidecar is left exactly in place.
    assert!(std::fs::symlink_metadata(&wal).is_ok());
}
