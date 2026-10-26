//! File publication through the public API, on every platform. The detailed
//! interruption and directory cases live in the Unix-only module tests.

use octopus_file_storage::publication::{publish, read_receipt, remove, PublicationOutcome, ReadGuard};

#[test]
fn files_publish_on_their_predecessor_and_refuse_a_stale_one() {
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let path = root.join("record.json");

    let created = publish(&path, "create", None, b"first").unwrap();
    assert_eq!(created.outcome, PublicationOutcome::Applied);
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    let first = created.after_hash.clone().unwrap();

    let replaced = publish(&path, "replace", Some(&first), b"second").unwrap();
    assert_eq!(replaced.outcome, PublicationOutcome::Applied);
    assert_eq!(std::fs::read(&path).unwrap(), b"second");

    let stale = publish(&path, "stale", Some(&first), b"third").unwrap();
    assert_eq!(stale.outcome, PublicationOutcome::NotApplied);
    assert_eq!(std::fs::read(&path).unwrap(), b"second");

    assert_eq!(read_receipt(&path, "replace").unwrap(), Some(replaced.clone()));
    // A repeated operation id answers with its first receipt.
    assert_eq!(publish(&path, "replace", Some(&first), b"second").unwrap(), replaced);

    let guard = ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).unwrap();
    guard.verify().unwrap();
    drop(guard);

    let removed = remove(&path, "remove", replaced.after_hash.as_deref().unwrap()).unwrap();
    assert_eq!(removed.outcome, PublicationOutcome::Applied);
    assert!(!path.exists());
}
