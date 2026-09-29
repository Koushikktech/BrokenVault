use brokenvault::core::chunker::{CHUNK_MAX_SIZE, CHUNK_MIN_SIZE, chunk_slice};
use brokenvault::core::errors::ManifestError;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::HashSet;

#[test]
fn test_chunker_empty_data() {
    let chunks = chunk_slice(&[]).unwrap();
    assert!(chunks.is_empty());
}

#[test]
fn test_chunker_small_data() {
    let data = b"Hello, BrokenVault!";
    let chunks = chunk_slice(data).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len, data.len() as u64);
    assert_eq!(chunks[0].offset, 0);
}

#[test]
fn test_chunker_bounds_and_determinism() {
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let mut data = vec![0u8; 1024 * 1024];
    rng.fill_bytes(&mut data);

    let chunks1 = chunk_slice(&data).unwrap();
    let chunks2 = chunk_slice(&data).unwrap();

    assert_eq!(chunks1, chunks2);
    assert!(!chunks1.is_empty());

    let mut total_len = 0;
    for chunk in &chunks1 {
        assert!(chunk.len >= CHUNK_MIN_SIZE as u64 || chunk.len == data.len() as u64);
        assert!(chunk.len <= CHUNK_MAX_SIZE as u64);
        assert_eq!(chunk.offset, total_len);
        total_len += chunk.len;
    }
    assert_eq!(total_len, data.len() as u64);
}

#[test]
fn test_chunker_shift_resilience() {
    let mut rng = ChaCha8Rng::seed_from_u64(1337);
    let mut original = vec![0u8; 512 * 1024];
    rng.fill_bytes(&mut original);

    let chunks_orig = chunk_slice(&original).unwrap();
    let set_orig: HashSet<_> = chunks_orig.iter().map(|c| &c.hash).collect();

    let mut shifted = Vec::with_capacity(original.len() + 1);
    shifted.push(0xAA);
    shifted.extend_from_slice(&original);

    let chunks_shifted = chunk_slice(&shifted).unwrap();
    let set_shifted: HashSet<_> = chunks_shifted.iter().map(|c| &c.hash).collect();

    let common = set_orig.intersection(&set_shifted).count();
    let ratio = (common as f64) / (set_orig.len() as f64);

    assert!(
        ratio >= 0.85,
        "CDC shift resilience ratio too low: {} ({}/{})",
        ratio,
        common,
        set_orig.len()
    );
}

#[test]
fn test_manifest_canonical_and_validation() {
    let entries = vec![
        ManifestEntry::Dir {
            path: "exports".to_string(),
            mtime: (1767225600, 0),
        },
        ManifestEntry::File {
            path: "empty.bin".to_string(),
            size: 0,
            mtime: (1767225602, 0),
            chunks: Vec::new(),
        },
        ManifestEntry::File {
            path: "notes.txt".to_string(),
            size: 12,
            mtime: (1767225601, 123000000),
            chunks: vec![(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
                12,
            )],
        },
    ];

    let manifest = Manifest::new(entries).unwrap();
    let id1 = manifest.manifest_id().unwrap();
    let bytes = manifest.canonical_bytes().unwrap();

    let deserialized = Manifest::from_bytes(&bytes).unwrap();
    let id2 = deserialized.manifest_id().unwrap();

    assert_eq!(id1, id2);
    assert_eq!(manifest, deserialized);
    assert_eq!(manifest.total_bytes(), 12);
    assert_eq!(manifest.total_files(), 2);
    assert_eq!(manifest.total_dirs(), 1);
    assert_eq!(manifest.total_chunks(), 1);
}

#[test]
fn test_manifest_chunk_sum_mismatch() {
    let entries = vec![ManifestEntry::File {
        path: "mismatch.txt".to_string(),
        size: 100,
        mtime: (1000, 0),
        chunks: vec![("hash1".to_string(), 50)],
    }];
    assert!(matches!(
        Manifest::new(entries),
        Err(ManifestError::ChunkSumMismatch { .. })
    ));
}

#[test]
fn test_manifest_duplicate_entry() {
    let entries = vec![
        ManifestEntry::Dir {
            path: "duplicate".to_string(),
            mtime: (1000, 0),
        },
        ManifestEntry::Dir {
            path: "duplicate".to_string(),
            mtime: (2000, 0),
        },
    ];
    assert!(Manifest::new(entries).is_err());
}
