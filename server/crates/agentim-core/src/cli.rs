use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::serve::ListenerExt;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::config::Config;
use crate::hub::Hub;
use crate::logic::HubConfig;
use crate::server::{AppState, router};

/// AgentIM server (both products share this command line).
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
    /// enable password-less login by name. For local development only.
    #[arg(long)]
    demo: bool,
    /// Allow --demo on an address other than localhost (anyone who reaches
    /// it can log in as anyone).
    #[arg(long)]
    demo_public: bool,
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
    /// Encryption at rest (docs/04-security-compliance.md §2.6).
    Encryption {
        #[command(subcommand)]
        action: EncCmd,
    },
    /// Open API keys (docs/07-open-api.md §5).
    ApiKey {
        #[command(subcommand)]
        action: KeyCmd,
    },
    /// Agents (docs/03-agents.md §2.1–§2.3). Works while the server
    /// runs; revoke keys in the app or the open API so connections close.
    Agent {
        #[command(subcommand)]
        action: AgentCmd,
    },
    /// The cluster (docs/11 §7). Works while the nodes run; reads and
    /// writes the cluster tables only.
    Cluster {
        #[command(subcommand)]
        action: ClusterCmd,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ClusterCmd {
    /// Nodes (state, last heartbeat, version) and who holds the shards of
    /// each database.
    Status,
    /// Takes a node out of service: it hands every shard to the next
    /// preferred node, reports not ready and sends its clients elsewhere.
    /// Stop the process afterwards; starting it again brings it back.
    Drain {
        #[arg(long)]
        node: String,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Creates an agent owned by a person and prints its key once, with
    /// ready-to-paste MCP and WebSocket settings.
    Create {
        #[arg(long)]
        name: String,
        /// The person responsible for the agent (user name).
        #[arg(long)]
        owner: String,
        /// How agents reach the server, e.g. https://im.example.com
        /// (default: server.public_url).
        #[arg(long)]
        url: Option<String>,
        /// A team agent: it stays when its owner leaves a conversation or
        /// is deactivated (docs/03-agents.md §1.2).
        #[arg(long)]
        team: bool,
    },
    /// Gives a team agent a new owner (in the same unit).
    Transfer {
        #[arg(long)]
        name: String,
        /// The new owner (user name).
        #[arg(long)]
        owner: String,
        /// Revoke the agent's keys (the previous owner may hold copies);
        /// the new owner then issues new ones with `agent key`.
        #[arg(long)]
        rotate_keys: bool,
    },
    /// Issues another key for an agent (e.g. for a new environment).
    Key {
        #[arg(long)]
        name: String,
        #[arg(long)]
        url: Option<String>,
    },
    /// Lists a person's agents and their keys (never the secrets).
    List {
        #[arg(long)]
        owner: String,
    },
    /// Writes an agent package (JSON) to move the agent to another server
    /// (docs/03-agents.md §7.3): profile, checkpoints, summaries; never keys.
    Export {
        #[arg(long)]
        name: String,
        /// Also the conversation content the agent sees, from the
        /// conversations its owner is in too.
        #[arg(long)]
        with_conversations: bool,
        /// File to write (default: standard output).
        #[arg(long)]
        out: Option<String>,
    },
    /// Creates an agent from a package and prints its new key once.
    Import {
        /// The package file.
        file: String,
        /// The person responsible for the new agent.
        #[arg(long)]
        owner: String,
        /// A new name (default: the one in the package).
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum EncCmd {
    /// Encrypts content stored before encryption was on. Run while the
    /// server is stopped; safe to repeat.
    EncryptExisting,
    /// Turns encrypted content back into plaintext after setting
    /// `encryption.mode = "off"` (the master key must still be configured).
    /// Run while the server is stopped; safe to repeat.
    DecryptExisting,
    /// Re-wraps the data keys with a new master key, then prints what to
    /// change. Run while the server is stopped.
    RotateKey {
        /// File with the new base64 master key; created when missing.
        #[arg(long)]
        new_key_file: String,
    },
    /// Prints a new random master key (base64).
    GenerateKey,
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Creates a key and prints it once.
    Create {
        #[arg(long)]
        name: String,
        /// Comma separated, e.g. users:write,messages:write
        #[arg(long)]
        scopes: String,
        /// Addresses or networks allowed to use the key, comma separated.
        #[arg(long)]
        ip_allow: Option<String>,
        #[arg(long)]
        ttl_days: Option<i64>,
    },
    List,
    Revoke {
        key_id: String,
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
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }
}

/// Runs the server or a command.
/// Joins a cluster with an opened (deferred) hub and starts the node; the
/// `agentim-cluster` crate provides it (docs/11).
pub type ClusterJoin = fn(Hub) -> crate::cluster::BoxFuture<Result<(), String>>;

/// `cluster` commands, run without opening the hub.
pub type ClusterAdmin = fn(Config, ClusterCmd) -> crate::cluster::BoxFuture<Result<(), String>>;

/// What the `agentim-cluster` crate provides.
#[derive(Clone, Copy)]
pub struct ClusterSupport {
    pub join: ClusterJoin,
    pub admin: ClusterAdmin,
}

pub async fn run() {
    run_with(None).await
}

pub async fn run_with(cluster: Option<ClusterSupport>) {
    let cmd = Args::command().name("agentim").about("AgentIM server");
    let args = Args::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());
    init_logging(&args.log_format);
    if args.demo && args.cmd.is_none() && !args.addr.ip().is_loopback() && !args.demo_public {
        eprintln!(
            "--demo lets anyone log in as anyone; it listens on localhost only \
             (add --demo-public to listen on {})",
            args.addr
        );
        std::process::exit(2)
    }
    let cfg = match &args.config {
        Some(p) => Config::load(p).unwrap_or_else(|e| {
            eprintln!("config: {e}");
            std::process::exit(2)
        }),
        None => Config::single_sqlite(&args.db),
    };
    if let Some(Cmd::Cluster { action }) = &args.cmd {
        let Some(c) = cluster else {
            eprintln!("this build has no cluster support");
            std::process::exit(1)
        };
        if let Err(e) = (c.admin)(cfg, action.clone()).await {
            eprintln!("cluster: {e}");
            std::process::exit(1)
        }
        return;
    }
    if let Some(Cmd::Encryption {
        action: EncCmd::GenerateKey,
    }) = &args.cmd
    {
        println!("{}", crate::crypto::Kek::generate_base64());
        return;
    }
    let hub_cfg = HubConfig {
        stream_idle: Duration::from_secs(args.stream_idle_secs),
        takeover_idle: Duration::from_secs(cfg.agents.takeover_idle_minutes * 60),
        sync: cfg.sync_config(),
        guards: crate::guard::GuardConfig {
            // The demo's agent ping-pong stops quickly enough to watch.
            root_messages: if args.demo { 12 } else { 50 },
            ..Default::default()
        },
        ..Default::default()
    };
    let clustered = cfg.cluster.enabled;
    let hub = match Hub::open_deferred(cfg, hub_cfg).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(code = e.code, error = e.message, "startup failed");
            std::process::exit(1)
        }
    };
    if clustered {
        let Some(ClusterSupport { join, .. }) = cluster else {
            tracing::error!("this build has no cluster support");
            std::process::exit(1)
        };
        if let Err(e) = join(hub.clone()).await {
            tracing::error!(error = e, "joining the cluster failed");
            std::process::exit(1)
        }
    }
    if let Err(e) = hub.finish_open().await {
        tracing::error!(code = e.code, error = e.message, "startup failed");
        std::process::exit(1)
    }

    if let Some(Cmd::Encryption { action }) = &args.cmd {
        let r = match action {
            EncCmd::GenerateKey => unreachable!("handled before opening"),
            EncCmd::EncryptExisting => hub.encrypt_existing().await,
            EncCmd::DecryptExisting => hub.decrypt_existing().await,
            EncCmd::RotateKey { new_key_file } => {
                let key = match std::fs::read_to_string(new_key_file) {
                    Ok(k) => k.trim().to_string(),
                    Err(_) => {
                        let k = crate::crypto::Kek::generate_base64();
                        if let Err(e) = std::fs::write(new_key_file, &k) {
                            eprintln!("{new_key_file}: {e}");
                            std::process::exit(1)
                        }
                        k
                    }
                };
                hub.rotate_master_key(&key).await.map(|mut v| {
                    v["next"] = serde_json::json!(format!("set encryption.key_file = {new_key_file:?} (or replace the old key file with it) before starting the server"));
                    v
                })
            }
        };
        match r {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("encryption: {} {}", e.code, e.message);
                std::process::exit(1)
            }
        }
        return;
    }

    if let Some(Cmd::ApiKey { action }) = &args.cmd {
        let split = |s: &str| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        };
        let out = match action {
            KeyCmd::Create {
                name,
                scopes,
                ip_allow,
                ttl_days,
            } => hub
                .create_api_key(
                    name,
                    &split(scopes),
                    ttl_days.map(|d| d * 86_400_000),
                    ip_allow.as_deref().map(split),
                )
                .await
                .map(|(secret, row)| {
                    let mut v = row.view();
                    v["key"] = serde_json::json!(secret);
                    v["note"] = serde_json::json!("store the key now; it is not shown again");
                    v
                }),
            KeyCmd::List => Ok(serde_json::json!(
                hub.api_keys().iter().map(|k| k.view()).collect::<Vec<_>>()
            )),
            KeyCmd::Revoke { key_id } => hub.revoke_api_key(key_id).await.and_then(|ok| {
                if ok {
                    Ok(serde_json::json!({"revoked": key_id}))
                } else {
                    Err(crate::model::ApiError::new(
                        "not_found",
                        "no such active key",
                    ))
                }
            }),
        };
        match out {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("api-key: {} {}", e.code, e.message);
                std::process::exit(1)
            }
        }
        return;
    }

    if let Some(Cmd::Agent { action }) = &args.cmd {
        let owner_of = |name: &str| -> crate::model::ApiResult<crate::model::User> {
            hub.user_by_name(name)
                .filter(|u| u.kind == crate::model::UserKind::Human)
                .ok_or_else(|| {
                    crate::model::ApiError::new("not_found", format!("no person named {name}"))
                })
        };
        let out = match action {
            AgentCmd::Create {
                name,
                owner,
                url,
                team,
            } => match owner_of(owner) {
                Ok(o) => hub.create_agent_with(&o, name, *team).await.map(|mut v| {
                    v["connect"] =
                        hub.connect_settings(v["key"].as_str().unwrap_or(""), url.as_deref());
                    v["note"] = serde_json::json!("store the key now; it is not shown again");
                    v
                }),
                Err(e) => Err(e),
            },
            AgentCmd::Key { name, url } => {
                let agent = hub
                    .user_by_name(name)
                    .filter(|u| u.kind == crate::model::UserKind::Agent);
                match agent.and_then(|a| {
                    a.owner_id
                        .clone()
                        .and_then(|o| hub.user(&o))
                        .map(|o| (a, o))
                }) {
                    Some((a, o)) => hub.agent_key(&o, &a.id, false).await.map(|mut v| {
                        v["connect"] =
                            hub.connect_settings(v["key"].as_str().unwrap_or(""), url.as_deref());
                        v["note"] = serde_json::json!("store the key now; it is not shown again");
                        v
                    }),
                    None => Err(crate::model::ApiError::new(
                        "not_found",
                        format!("no agent named {name}"),
                    )),
                }
            }
            AgentCmd::Transfer {
                name,
                owner,
                rotate_keys,
            } => match (
                hub.user_by_name(name)
                    .filter(|u| u.kind == crate::model::UserKind::Agent),
                owner_of(owner),
            ) {
                (Some(a), Ok(o)) => hub.transfer_agent(&a.id, &o.id, *rotate_keys).await,
                (None, _) => Err(crate::model::ApiError::new(
                    "not_found",
                    format!("no agent named {name}"),
                )),
                (_, Err(e)) => Err(e),
            },
            AgentCmd::List { owner } => match owner_of(owner) {
                Ok(o) => hub.list_agents(&o).await,
                Err(e) => Err(e),
            },
            AgentCmd::Export {
                name,
                with_conversations,
                out,
            } => {
                let agent = hub
                    .user_by_name(name)
                    .filter(|u| u.kind == crate::model::UserKind::Agent);
                let res = match agent.and_then(|a| {
                    a.owner_id
                        .clone()
                        .and_then(|o| hub.user(&o))
                        .map(|o| (a, o))
                }) {
                    Some((a, o)) => hub.export_agent(&o, &a.id, *with_conversations).await,
                    None => Err(crate::model::ApiError::new(
                        "not_found",
                        format!("no agent named {name}"),
                    )),
                };
                match (res, out) {
                    (Ok(pkg), Some(path)) => std::fs::write(path, pkg.to_string())
                        .map(|_| serde_json::json!({"written": path, "sha256": pkg["sha256"]}))
                        .map_err(|e| crate::model::ApiError::new("io", e.to_string())),
                    (r, None) => r,
                    (Err(e), _) => Err(e),
                }
            }
            AgentCmd::Import { file, owner, name } => {
                let pkg = std::fs::read_to_string(file)
                    .map_err(|e| crate::model::ApiError::new("io", e.to_string()))
                    .and_then(|t| {
                        serde_json::from_str::<serde_json::Value>(&t)
                            .map_err(|e| crate::model::ApiError::new("bad_request", e.to_string()))
                    });
                match (pkg, owner_of(owner)) {
                    (Ok(p), Ok(o)) => {
                        hub.import_agent(&o, &p, name.as_deref())
                            .await
                            .map(|mut v| {
                                v["note"] =
                                    serde_json::json!("store the key now; it is not shown again");
                                v
                            })
                    }
                    (Err(e), _) | (_, Err(e)) => Err(e),
                }
            }
        };
        match out {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("agent: {} {}", e.code, e.message);
                std::process::exit(1)
            }
        }
        return;
    }

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
        && let Err(e) = crate::demo::seed(&hub).await
    {
        tracing::error!(error = e.message, "demo seed failed");
        std::process::exit(1)
    }
    hub.spawn_background();
    if args.demo {
        crate::demo::spawn_agents(&hub);
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
    let tls = hub.config().server.tls.as_ref().map(|t| {
        crate::tls::server_config(&t.cert, &t.key).unwrap_or_else(|e| {
            tracing::error!(error = e, "tls");
            std::process::exit(2)
        })
    });
    let scheme = if tls.is_some() { "https" } else { "http" };
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
    tracing::info!(addr = %args.addr, demo = args.demo, "AgentIM listening");
    println!("AgentIM listening on {scheme}://{}", args.addr);
    if args.demo {
        println!("Demo mode: open the URL above and log in as alice, bob or carol.");
    }
    // Client addresses feed the open API's address rules.
    match tls {
        Some(cfg) => {
            let l = crate::tls::TlsListener::new(listener, cfg).expect("tls listener");
            let app = app.into_make_service_with_connect_info::<crate::tls::PeerAddr>();
            axum::serve(l, app).await.expect("serve")
        }
        None => {
            // Small request/response frames: Nagle + delayed ACK would add
            // ~40ms per round trip.
            let l = listener.tap_io(|tcp| {
                let _ = tcp.set_nodelay(true);
            });
            let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
            axum::serve(l, app).await.expect("serve")
        }
    }
}
