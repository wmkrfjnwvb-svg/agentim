use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::serve::ListenerExt;
use clap::{Parser, Subcommand};

use agentim::config::Config;
use agentim::hub::Hub;
use agentim::logic::HubConfig;
use agentim::server::{AppState, router};

/// AgentIM server.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Configuration file (databases, units, writer). Overrides --db.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Single SQLite database file (when no --config is given).
    #[arg(long, default_value = "agentim.db", global = true)]
    db: String,
    /// Listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: SocketAddr,
    /// Seed demo users and conversations, run the scripted demo agents and
    /// enable password-less login by name.
    #[arg(long)]
    demo: bool,
    /// Seconds without a chunk before a stream is marked failed(timeout).
    #[arg(long, default_value_t = 30)]
    stream_idle_secs: u64,
    /// Log format: text or json.
    #[arg(long, default_value = "text", global = true)]
    log_format: String,
    /// Require this bearer token on /metrics.
    #[arg(long, env = "AGENTIM_METRICS_TOKEN")]
    metrics_token: Option<String>,
    /// Keep running after a storage failure instead of exiting for a restart.
    #[arg(long)]
    no_exit_on_storage_failure: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Move every conversation of a unit into the database the configuration
    /// now assigns to it. Run while the server is stopped.
    MigrateUnit {
        #[arg(long)]
        unit: String,
    },
}

fn init_logging(format: &str) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    init_logging(&args.log_format);
    let cfg = match &args.config {
        Some(p) => Config::load(p).unwrap_or_else(|e| {
            eprintln!("config: {e}");
            std::process::exit(2)
        }),
        None => Config::single_sqlite(&args.db),
    };
    let hub_cfg = HubConfig {
        stream_idle: Duration::from_secs(args.stream_idle_secs),
        open_stats: args.demo,
        ..Default::default()
    };
    let hub = match Hub::open(cfg, hub_cfg).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(code = e.code, error = e.message, "startup failed");
            std::process::exit(1)
        }
    };

    if let Some(Cmd::MigrateUnit { unit }) = args.cmd {
        match hub.migrate_unit(&unit).await {
            Ok(v) => println!("{v}"),
            Err(e) => {
                eprintln!("migrate-unit failed: {} {}", e.code, e.message);
                std::process::exit(1)
            }
        }
        return;
    }

    if args.demo
        && let Err(e) = agentim::demo::seed(&hub).await
    {
        tracing::error!(error = e.message, "demo seed failed");
        std::process::exit(1)
    }
    hub.spawn_background();
    if args.demo {
        agentim::demo::spawn_agents(&hub);
    }
    if !args.no_exit_on_storage_failure {
        // A database that failed a commit must not keep serving; exit so the
        // supervisor restarts the process and the database recovers its log.
        let h = hub.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Some(r) = h.storage_failed() {
                    tracing::error!(reason = r, "storage failure, exiting for restart");
                    std::process::exit(70);
                }
            }
        });
    }
    let app = router(AppState {
        hub,
        demo_login: args.demo,
        metrics_token: args.metrics_token,
    });
    let listener = tokio::net::TcpListener::bind(args.addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "bind failed");
            std::process::exit(1)
        });
    // Small request/response frames: Nagle + delayed ACK would add ~40ms per round trip.
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    tracing::info!(addr = %args.addr, demo = args.demo, "AgentIM listening");
    println!("AgentIM listening on http://{}", args.addr);
    if args.demo {
        println!("Demo mode: open the URL above and log in as alice, bob or carol.");
    }
    axum::serve(listener, app).await.expect("serve");
}
