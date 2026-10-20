use super::*;

#[test]
fn writer_candidates_refresh_exact_generations_on_directory_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("calls");
    let mut index = WriterCallIndex::default();
    assert!(index.candidates(&root, 4).unwrap().is_empty());
    std::fs::create_dir(&root).unwrap();
    let previous = root.join("previous.artifact.json");
    let current = root.join("current.artifact.json");
    let ignored = root.join("unrelated.artifact.json");
    for path in [&previous, &current, &ignored] {
        std::fs::write(path, b"{}").unwrap();
    }
    assert_eq!(index.candidates(&root, 4).unwrap().len(), 3);
    index.record(previous.clone(), Some(3));
    index.record(current.clone(), Some(4));
    index.record(ignored, None);
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![current.clone()]);
    assert_eq!(index.candidates(&root, 3).unwrap(), vec![previous]);
    let fingerprint = index.directories[&root].fingerprint.clone();
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![current.clone()]);
    assert_eq!(index.directories[&root].fingerprint, fingerprint);
    std::fs::remove_file(&current).unwrap();
    let fresh = root.join("fresh.artifact.json");
    std::fs::write(&fresh, b"{}").unwrap();
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![fresh.clone()]);
    assert!(!index.generations.contains_key(&current));
    index.record(fresh.clone(), Some(4));
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![fresh]);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(index.candidates(&root, 4).unwrap().is_empty());
    std::fs::write(&root, b"file").unwrap();
    assert!(index.candidates(&root, 4).is_err());
}
