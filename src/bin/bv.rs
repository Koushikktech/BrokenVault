use anyhow::{Context, Result};
use brokenvault::client::devtools::{diff_directories, generate_sample_dataset};
use brokenvault::client::upload::{UploadOptions, run_backup};
use brokenvault::core::proto::{OpenUploadSummary, VersionSummary};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "bv", about = "BrokenVault Client")]
struct Cli {
    #[arg(long, env = "BV_SERVER", default_value = "http://127.0.0.1:7878")]
    server: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Backup {
        src: PathBuf,

        #[arg(long, default_value_t = 8)]
        jobs: usize,

        #[arg(long)]
        stop_after_chunks: Option<usize>,
    },
    List {
        #[arg(long)]
        all: bool,

        #[arg(long)]
        json: bool,
    },
    Abort {
        upload_id: String,
    },
    Dev {
        #[command(subcommand)]
        sub: DevCommands,
    },
}

#[derive(Subcommand, Debug)]
enum DevCommands {
    Gen {
        dir: PathBuf,

        #[arg(long, default_value_t = 1)]
        seed: u64,

        #[arg(long)]
        mutate: bool,
    },
    Diff {
        a: PathBuf,
        b: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let server_url = cli.server.trim_end_matches('/');

    match cli.command {
        Commands::Backup {
            src,
            jobs,
            stop_after_chunks,
        } => {
            let options = UploadOptions {
                jobs,
                stop_after_chunks,
            };
            if let Err(e) = run_backup(&src, server_url, options) {
                if matches!(e, brokenvault::core::errors::CoreError::Interrupted) {
                    std::process::exit(130);
                }
                return Err(e).context("backup failed");
            }
        }
        Commands::List { all, json } => {
            let client = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent();

            let versions_url = format!("{}/v1/versions", server_url);
            let mut res = client
                .get(&versions_url)
                .call()
                .context("failed to query server for versions")?;

            if res.status().as_u16() != 200 {
                eprintln!("Failed to list versions: HTTP {}", res.status());
                std::process::exit(3);
            }

            let versions: Vec<VersionSummary> = res
                .body_mut()
                .read_json()
                .context("failed to parse version list")?;

            let mut open_uploads: Vec<OpenUploadSummary> = Vec::new();
            if all {
                let uploads_url = format!("{}/v1/uploads", server_url);
                if let Ok(mut up_res) = client.get(&uploads_url).call() {
                    if up_res.status().as_u16() == 200 {
                        open_uploads = up_res.body_mut().read_json().unwrap_or_default();
                    }
                }
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "versions": versions,
                        "open_uploads": open_uploads,
                    }))?
                );
            } else {
                if versions.is_empty() && open_uploads.is_empty() {
                    println!("No versions stored.");
                    return Ok(());
                }

                for v in &versions {
                    println!(
                        "✔ {} (manifest={}) total={} uploaded={} reused={}",
                        v.id,
                        &v.manifest_id[..12.min(v.manifest_id.len())],
                        v.total_bytes,
                        v.uploaded_bytes,
                        v.reused_bytes
                    );
                }

                if all {
                    for up in &open_uploads {
                        println!(
                            "⏸ {} [UNFINISHED] total={} chunks={}/{}",
                            up.upload_id, up.total_bytes, up.chunks_present, up.chunks_total
                        );
                    }
                }
            }
        }
        Commands::Abort { upload_id } => {
            let client = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent();

            let abort_url = format!("{}/v1/uploads/{}", server_url, upload_id);
            let res = client
                .delete(&abort_url)
                .call()
                .context("failed to reach server")?;

            if res.status().as_u16() == 204 {
                println!("Upload {} successfully aborted.", upload_id);
            } else {
                eprintln!(
                    "Failed to abort upload {}: HTTP {}",
                    upload_id,
                    res.status()
                );
                std::process::exit(2);
            }
        }
        Commands::Dev { sub } => match sub {
            DevCommands::Gen { dir, seed, mutate } => {
                generate_sample_dataset(&dir, seed, mutate)
                    .context("failed to generate dataset")?;
                println!(
                    "Generated dataset at {} (seed={}, mutate={})",
                    dir.display(),
                    seed,
                    mutate
                );
            }
            DevCommands::Diff { a, b } => {
                let identical = diff_directories(&a, &b).context("failed to diff directories")?;
                if identical {
                    println!(
                        "✔ EXACT MATCH: {} and {} are identical",
                        a.display(),
                        b.display()
                    );
                } else {
                    println!(
                        "✖ MISMATCH: differences found between {} and {}",
                        a.display(),
                        b.display()
                    );
                    std::process::exit(1);
                }
            }
        },
    }

    Ok(())
}
