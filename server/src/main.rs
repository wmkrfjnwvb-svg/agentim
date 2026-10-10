use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;

use agentim::hub::{Hub, HubConfig};
use agentim::server::{AppState, router};
use agentim::store::Store;

/// AgentIM single-node server.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: SocketAddr,
    /// SQLite database file.
    #[arg(long, default_value = "agentim.db")]
    db: String,
    /// Seed demo users and conversations, run the scripted demo agents and
    /// enable password-less login by name.
    #[arg(long)]
    demo: bool,
    /// Seconds without a chunk before a stream is marked failed(timeout).
    #[arg(long, default_value_t = 30)]
    stream_idle_secs: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let mut store = Store::open(&args.db).expect("open database");
    if args.demo {
        agentim::demo::seed(&mut store).expect("seed demo data");
    }
    let cfg = HubConfig {
        stream_idle: Duration::from_secs(args.stream_idle_secs),
        ..Default::default()
    };
    let hub = Hub::new(store, cfg);
    hub.spawn_background();
    if args.demo {
        agentim::demo::spawn_agents(&hub);
    }
    let app = router(AppState {
        hub,
        demo_login: args.demo,
    });
    let listener = tokio::net::TcpListener::bind(args.addr)
        .await
        .expect("bind");
    println!("AgentIM listening on http://{}", args.addr);
    if args.demo {
        println!("Demo mode: open the URL above and log in as alice, bob or carol.");
    }
    axum::serve(listener, app).await.expect("serve");
}
