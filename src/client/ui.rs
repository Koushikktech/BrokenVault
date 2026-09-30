pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn print_completed_summary(
    version: &str,
    total_bytes: u64,
    uploaded_bytes: u64,
    reused_bytes: u64,
) {
    let uploaded_pct = if total_bytes > 0 {
        (uploaded_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let reused_pct = if total_bytes > 0 {
        (reused_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        100.0
    };

    println!("✔ COMPLETED   version={}", version);
    println!(
        "   total {} │ uploaded {} ({:.2}%) │ reused {} ({:.2}%)",
        format_bytes(total_bytes),
        format_bytes(uploaded_bytes),
        uploaded_pct,
        format_bytes(reused_bytes),
        reused_pct
    );
}

pub fn print_unchanged_summary(version: &str, total_bytes: u64) {
    println!(
        "✔ UNCHANGED   version={} (identical to {}, no new version created)",
        version, version
    );
    println!(
        "   total {} │ uploaded 0 B (0.00%) │ reused {} (100.00%)",
        format_bytes(total_bytes),
        format_bytes(total_bytes)
    );
}

pub fn print_paused_summary(upload_id: &str, present: usize, total: usize) {
    println!(
        "⏸ PAUSED   upload={}  state=UNFINISHED (hidden from `bv list`)",
        upload_id
    );
    println!(
        "   Server holds {} / {} chunks. Re-run the same command to continue.",
        present, total
    );
}

pub fn print_vault_stats(stats: &crate::core::proto::VaultStats) {
    println!("Vault ID:             {}", stats.vault_id);
    println!(
        "Total Versions:       {} completed, {} open",
        stats.completed_versions, stats.open_uploads
    );
    println!(
        "Logical Data Stored:  {}",
        format_bytes(stats.total_logical_bytes)
    );
    println!(
        "Physical Disk Used:   {} across {} unique chunks",
        format_bytes(stats.total_physical_chunk_bytes),
        stats.unique_chunks
    );
    println!("Deduplication Ratio:  {:.2}x", stats.deduplication_ratio);
    println!(
        "Space Saved:          {} ({:.1}%)",
        format_bytes(stats.space_saved_bytes),
        stats.space_saved_percent
    );
}

pub fn print_version_diff(report: &crate::core::proto::VersionDiffReport) {
    println!(
        "=== Comparing {} -> {} ===",
        report.from_version, report.to_version
    );
    if report.files.is_empty() {
        println!("No changes between versions.");
        return;
    }

    for file in &report.files {
        match file.change {
            crate::core::proto::DiffChangeType::Added => {
                let size_str = file
                    .new_size
                    .map(format_bytes)
                    .unwrap_or_else(|| "dir".to_string());
                println!(
                    "  + Added:    {} ({}, {} new / {} reused chunks)",
                    file.path, size_str, file.new_chunks, file.reused_chunks
                );
            }
            crate::core::proto::DiffChangeType::Removed => {
                let size_str = file
                    .old_size
                    .map(format_bytes)
                    .unwrap_or_else(|| "dir".to_string());
                println!("  - Removed:  {} ({})", file.path, size_str);
            }
            crate::core::proto::DiffChangeType::Modified => {
                let old_str = file
                    .old_size
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                let new_str = file
                    .new_size
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  ~ Modified: {} ({} -> {}, {} new / {} reused chunks)",
                    file.path, old_str, new_str, file.new_chunks, file.reused_chunks
                );
            }
        }
    }

    println!();
    println!(
        "Summary: +{} added, -{} removed, ~{} modified │ new data: {} │ reused data: {}",
        report.files_added,
        report.files_removed,
        report.files_modified,
        format_bytes(report.total_new_bytes),
        format_bytes(report.total_reused_bytes)
    );
}
