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
