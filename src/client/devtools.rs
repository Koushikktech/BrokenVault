use crate::core::errors::CoreError;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use walkdir::WalkDir;

pub fn generate_sample_dataset(
    dir: impl AsRef<Path>,
    seed: u64,
    mutate: bool,
) -> Result<(), CoreError> {
    let dir = dir.as_ref();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);

    if !mutate {
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        fs::create_dir_all(dir)?;

        fs::create_dir_all(dir.join("exports"))?;

        File::create(dir.join("empty.bin"))?;

        let mut notes = File::create(dir.join("notes.txt"))?;
        notes.write_all(b"BrokenVault verification notes. Deterministic seed dataset.\n")?;

        let docs_dir = dir.join("docs");
        fs::create_dir_all(&docs_dir)?;
        let mut readme = File::create(docs_dir.join("guide.md"))?;
        let mut readme_bytes = vec![0u8; 32 * 1024];
        rng.fill_bytes(&mut readme_bytes);
        readme.write_all(&readme_bytes)?;

        let bin_dir = dir.join("binaries");
        fs::create_dir_all(&bin_dir)?;
        let mut asset = File::create(bin_dir.join("asset.dat"))?;
        let mut asset_bytes = vec![0u8; 128 * 1024];
        rng.fill_bytes(&mut asset_bytes);
        asset.write_all(&asset_bytes)?;

        let mut large = File::create(bin_dir.join("large.bin"))?;
        let mut large_bytes = vec![0u8; 4 * 1024 * 1024];
        rng.fill_bytes(&mut large_bytes);
        large.write_all(&large_bytes)?;

        let deep_dir = dir.join("nested").join("deep").join("level");
        fs::create_dir_all(&deep_dir)?;
        let mut config = File::create(deep_dir.join("config.json"))?;
        config.write_all(b"{\"version\": 1, \"active\": true}\n")?;
    } else {
        let large_path = dir.join("binaries").join("large.bin");
        if large_path.exists() {
            let mut large = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&large_path)?;
            large.seek(SeekFrom::Start(1024 * 1024))?;
            let mut mutate_bytes = vec![0u8; 32 * 1024];
            rng.fill_bytes(&mut mutate_bytes);
            large.write_all(&mutate_bytes)?;
        }

        let mut new_file = File::create(dir.join("new_addition.txt"))?;
        new_file.write_all(b"This file was added in version 2 for deduplication testing.\n")?;
    }

    Ok(())
}

pub fn diff_directories(
    dir_a: impl AsRef<Path>,
    dir_b: impl AsRef<Path>,
) -> Result<bool, CoreError> {
    let dir_a = dir_a.as_ref();
    let dir_b = dir_b.as_ref();

    let items_a = collect_dir_items(dir_a)?;
    let items_b = collect_dir_items(dir_b)?;

    let paths_a: BTreeSet<&String> = items_a.keys().collect();
    let paths_b: BTreeSet<&String> = items_b.keys().collect();

    if paths_a != paths_b {
        let only_in_a: Vec<_> = paths_a.difference(&paths_b).collect();
        let only_in_b: Vec<_> = paths_b.difference(&paths_a).collect();
        if !only_in_a.is_empty() {
            eprintln!("Items only in A: {:?}", only_in_a);
        }
        if !only_in_b.is_empty() {
            eprintln!("Items only in B: {:?}", only_in_b);
        }
        return Ok(false);
    }

    for (rel_path, is_dir_a) in &items_a {
        let is_dir_b = items_b.get(rel_path).unwrap();
        if is_dir_a != is_dir_b {
            eprintln!(
                "Type mismatch for {}: dir in A={}, dir in B={}",
                rel_path, is_dir_a, is_dir_b
            );
            return Ok(false);
        }

        let path_a = dir_a.join(rel_path);
        let path_b = dir_b.join(rel_path);

        if !*is_dir_a {
            let meta_a = fs::metadata(&path_a)?;
            let meta_b = fs::metadata(&path_b)?;

            if meta_a.len() != meta_b.len() {
                eprintln!(
                    "Length mismatch for {}: A={} B={}",
                    rel_path,
                    meta_a.len(),
                    meta_b.len()
                );
                return Ok(false);
            }

            let mut f_a = File::open(&path_a)?;
            let mut f_b = File::open(&path_b)?;
            let mut buf_a = [0u8; 64 * 1024];
            let mut buf_b = [0u8; 64 * 1024];

            loop {
                let n_a = f_a.read(&mut buf_a)?;
                let n_b = f_b.read(&mut buf_b)?;
                if n_a != n_b || buf_a[..n_a] != buf_b[..n_b] {
                    eprintln!("Content mismatch for {}", rel_path);
                    return Ok(false);
                }
                if n_a == 0 {
                    break;
                }
            }

            let mtime_a = filetime::FileTime::from_last_modification_time(&meta_a).unix_seconds();
            let mtime_b = filetime::FileTime::from_last_modification_time(&meta_b).unix_seconds();
            if mtime_a.abs_diff(mtime_b) > 1 {
                eprintln!(
                    "Mtime mismatch for {}: A={} B={}",
                    rel_path, mtime_a, mtime_b
                );
                return Ok(false);
            }
        }
    }

    Ok(true)
}

fn collect_dir_items(root: &Path) -> Result<BTreeMap<String, bool>, CoreError> {
    let mut items = BTreeMap::new();
    for entry_res in WalkDir::new(root).sort_by_file_name() {
        let entry = entry_res.map_err(std::io::Error::other)?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let rel_path = path
            .strip_prefix(root)
            .map_err(std::io::Error::other)?
            .to_str()
            .ok_or_else(|| CoreError::Io(std::io::Error::other("non-utf8 path")))?
            .replace('\\', "/");

        items.insert(rel_path, entry.file_type().is_dir());
    }
    Ok(items)
}
