use crate::core::manifest::{Manifest, ManifestEntry};
use crate::core::proto::{DiffChangeType, FileDiff, VersionDiffReport};
use std::collections::{BTreeMap, HashSet};

pub fn compare_manifests(
    from_version: &str,
    from_manifest: &Manifest,
    to_version: &str,
    to_manifest: &Manifest,
) -> VersionDiffReport {
    let mut from_chunks = HashSet::new();
    let mut from_map = BTreeMap::new();
    for entry in &from_manifest.entries {
        from_map.insert(entry.path(), entry);
        if let ManifestEntry::File { chunks, .. } = entry {
            for (id, _) in chunks {
                from_chunks.insert(id.as_str());
            }
        }
    }

    let mut to_map = BTreeMap::new();
    for entry in &to_manifest.entries {
        to_map.insert(entry.path(), entry);
    }

    let mut files = Vec::new();
    let mut files_added = 0usize;
    let mut files_removed = 0usize;
    let mut files_modified = 0usize;
    let mut total_new_bytes = 0u64;
    let mut total_reused_bytes = 0u64;

    for (path, to_entry) in &to_map {
        match from_map.get(path) {
            None => {
                files_added = files_added.saturating_add(1);
                match to_entry {
                    ManifestEntry::Dir { .. } => {
                        files.push(FileDiff {
                            path: (*path).to_string(),
                            change: DiffChangeType::Added,
                            old_size: None,
                            new_size: None,
                            new_chunks: 0,
                            reused_chunks: 0,
                        });
                    }
                    ManifestEntry::File { size, chunks, .. } => {
                        let mut new_c = 0usize;
                        let mut reused_c = 0usize;
                        for (id, len) in chunks {
                            if from_chunks.contains(id.as_str()) {
                                reused_c = reused_c.saturating_add(1);
                                total_reused_bytes = total_reused_bytes.saturating_add(*len);
                            } else {
                                new_c = new_c.saturating_add(1);
                                total_new_bytes = total_new_bytes.saturating_add(*len);
                            }
                        }
                        files.push(FileDiff {
                            path: (*path).to_string(),
                            change: DiffChangeType::Added,
                            old_size: None,
                            new_size: Some(*size),
                            new_chunks: new_c,
                            reused_chunks: reused_c,
                        });
                    }
                }
            }
            Some(from_entry) => {
                let modified = match (from_entry, to_entry) {
                    (ManifestEntry::Dir { .. }, ManifestEntry::Dir { .. }) => false,
                    (
                        ManifestEntry::File {
                            size: s1,
                            chunks: c1,
                            ..
                        },
                        ManifestEntry::File {
                            size: s2,
                            chunks: c2,
                            ..
                        },
                    ) => s1 != s2 || c1 != c2,
                    _ => true,
                };

                if modified {
                    files_modified = files_modified.saturating_add(1);
                    match to_entry {
                        ManifestEntry::Dir { .. } => {
                            files.push(FileDiff {
                                path: (*path).to_string(),
                                change: DiffChangeType::Modified,
                                old_size: None,
                                new_size: None,
                                new_chunks: 0,
                                reused_chunks: 0,
                            });
                        }
                        ManifestEntry::File { size, chunks, .. } => {
                            let old_size = match from_entry {
                                ManifestEntry::File { size: s, .. } => Some(*s),
                                _ => None,
                            };
                            let mut new_c = 0usize;
                            let mut reused_c = 0usize;
                            for (id, len) in chunks {
                                if from_chunks.contains(id.as_str()) {
                                    reused_c = reused_c.saturating_add(1);
                                    total_reused_bytes = total_reused_bytes.saturating_add(*len);
                                } else {
                                    new_c = new_c.saturating_add(1);
                                    total_new_bytes = total_new_bytes.saturating_add(*len);
                                }
                            }
                            files.push(FileDiff {
                                path: (*path).to_string(),
                                change: DiffChangeType::Modified,
                                old_size,
                                new_size: Some(*size),
                                new_chunks: new_c,
                                reused_chunks: reused_c,
                            });
                        }
                    }
                } else if let ManifestEntry::File { chunks, .. } = to_entry {
                    for (id, len) in chunks {
                        if from_chunks.contains(id.as_str()) {
                            total_reused_bytes = total_reused_bytes.saturating_add(*len);
                        } else {
                            total_new_bytes = total_new_bytes.saturating_add(*len);
                        }
                    }
                }
            }
        }
    }

    for (path, from_entry) in &from_map {
        if !to_map.contains_key(path) {
            files_removed = files_removed.saturating_add(1);
            let old_size = match from_entry {
                ManifestEntry::File { size, .. } => Some(*size),
                _ => None,
            };
            files.push(FileDiff {
                path: (*path).to_string(),
                change: DiffChangeType::Removed,
                old_size,
                new_size: None,
                new_chunks: 0,
                reused_chunks: 0,
            });
        }
    }

    VersionDiffReport {
        from_version: from_version.to_string(),
        to_version: to_version.to_string(),
        files,
        files_added,
        files_removed,
        files_modified,
        total_new_bytes,
        total_reused_bytes,
    }
}
