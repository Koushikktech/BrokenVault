use anyhow::{Context, Result};
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::db::Database;
use brokenvault::server::debug::{DamageMode, apply_damage};
use brokenvault::server::store::Store;
use brokenvault::server::verify::{execute_verification, print_verify_report};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(name = "bvd", about = "BrokenVault Server Daemon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Serve {
        #[arg(long, env = "BV_DATA_DIR", default_value = "./vault")]
        data: PathBuf,

        #[arg(long, env = "BV_LISTEN", default_value = "127.0.0.1:7878")]
        listen: SocketAddr,
    },
    Verify {
        #[arg(long, default_value = "./vault")]
        data: PathBuf,

        #[arg(long)]
        json: bool,
    },
    Debug {
        #[command(subcommand)]
        sub: DebugCommands,
    },
}

#[derive(Subcommand, Debug)]
enum DebugCommands {
    Damage {
        #[arg(long, default_value = "./vault")]
        data: PathBuf,

        #[arg(long)]
        mode: DamageMode,

        #[arg(long)]
        chunk: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let command = cli.command.unwrap_or_else(|| {
        let data = std::env::var("BV_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./vault"));
        let listen = std::env::var("BV_LISTEN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| "127.0.0.1:7878".parse().unwrap());
        Commands::Serve { data, listen }
    });

    match command {
        Commands::Serve { data, listen } => {
            let store = Store::new(&data).context("failed to initialize store")?;
            let db_path = data.join("meta.db");
            let db = Database::open(&db_path).context("failed to open database")?;
            let state = AppState {
                store,
                db: Arc::new(Mutex::new(db)),
            };

            let app = create_router(state);
            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("failed to bind to {}", listen))?;

            println!("Server listening on http://{}", listen);

            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("server error")?;
        }
        Commands::Verify { data, json } => {
            let store = Store::open_read_only(&data).context("failed to open store")?;
            let db_path = data.join("meta.db");
            let db = Database::open_read_only(&db_path).context("failed to open database")?;
            let report = execute_verification(&store, &db).context("verification failed")?;

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_verify_report(&report);
            }

            if !report.healthy {
                std::process::exit(1);
            }
        }
        Commands::Debug { sub } => match sub {
            DebugCommands::Damage { data, mode, chunk } => {
                let store = Store::new(&data).context("failed to open store")?;
                let damaged_id = apply_damage(&store, mode, chunk.as_deref())
                    .context("failed to apply damage")?;
                println!("Damaged chunk {} using mode {:?}", damaged_id, mode);
            }
        },
    }

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
