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
    /// Keys the developer's backend signs client tokens with (docs/13
    /// §4.2). Works while the server runs.
    SigningKey {
        #[command(subcommand)]
        action: SigningCmd,
    },
    /// Client tokens for local development and tests.
    Token {
        #[command(subcommand)]
        action: TokenCmd,
    },
    /// Open API keys (docs/07-open-api.md §5).
    ApiKey {
        #[command(subcommand)]
        action: KeyCmd,
    },
    /// Keys for `agentim admin` (docs/12 §4), with a role. Works while
    /// the server runs.
    AdminKey {
        #[command(subcommand)]
        action: AdminKeyCmd,
    },
    /// The admin API of a running server (docs/12 §4): what the console
    /// does, from the command line.
    Admin {
        /// The server, e.g. https://im.example.com.
        #[arg(
            long,
            env = "AGENTIM_ADMIN_URL",
            default_value = "http://127.0.0.1:8080"
        )]
        url: String,
        /// An admin key (`agentim admin-key create`).
        #[arg(long, env = "AGENTIM_ADMIN_KEY")]
        key: String,
        #[command(subcommand)]
        action: crate::admin_cli::AdminCmd,
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
enum AdminKeyCmd {
    /// Creates a key and prints it once.
    Create {
        #[arg(long)]
        name: String,
        /// owner, developer, operator or moderator.
        #[arg(long, default_value = "owner")]
        role: String,
    },
    List,
    Revoke {
        key_id: String,
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
enum SigningCmd {
    /// Creates a key and prints its id and secret once.
    Create {
        #[arg(long)]
        name: String,
    },
    /// Lists the keys (never the secrets).
    List,
    /// Revokes a key: tokens signed with it stop working at once.
    Revoke { kid: String },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Signs a client token with the newest signing key, as the
    /// developer's backend would.
    Sign {
        /// The user id (the developer's).
        #[arg(long)]
        user: String,
        /// Lifetime in seconds.
        #[arg(long, default_value_t = 3600)]
        ttl: i64,
    },
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
            // The request's span carries trace_id and span_id (docs/14 §4.1).
            .with_current_span(true)
            .with_span_list(false)
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

/// The same commands for the admin API: their result instead of printing.
pub type ClusterQuery =
    fn(Config, ClusterCmd) -> crate::cluster::BoxFuture<Result<serde_json::Value, String>>;

/// What the `agentim-cluster` crate provides.
#[derive(Clone, Copy)]
pub struct ClusterSupport {
    pub join: ClusterJoin,
    pub admin: ClusterAdmin,
    pub query: ClusterQuery,
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
    let mut cfg = match &args.config {
        Some(p) => Config::load(p).unwrap_or_else(|e| {
            eprintln!("config: {e}");
            std::process::exit(2)
        }),
        None => Config::single_sqlite(&args.db),
    };
    if args.demo {
        // The demo shows people creating their own agents.
        cfg.clients.allow.create_agents = true;
    }
    if let Some(Cmd::Admin { url, key, action }) = &args.cmd {
        match crate::admin_cli::run(url, key, action).await {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("admin: {e}");
                std::process::exit(1)
            }
        }
        return;
    }
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
        let Some(ClusterSupport { join, query, .. }) = cluster else {
            tracing::error!("this build has no cluster support");
            std::process::exit(1)
        };
        if let Err(e) = join(hub.clone()).await {
            tracing::error!(error = e, "joining the cluster failed");
            std::process::exit(1)
        }
        hub.set_cluster_query(query);
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

    if let Some(Cmd::SigningKey { action }) = &args.cmd {
        let out = match action {
            SigningCmd::Create { name } => hub.create_signing_key(name).await.map(|mut v| {
                v["note"] = serde_json::json!(
                    "store the secret in your backend now; it is not shown again. Sign client tokens with HS256, header kid = this kid (docs/13 §4.2)"
                );
                v
            }),
            SigningCmd::List => hub.signing_keys().await.map(|v| serde_json::json!(v)),
            SigningCmd::Revoke { kid } => hub
                .revoke_signing_key(kid)
                .await
                .map(|_| serde_json::json!({"revoked": kid})),
        };
        match out {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("signing-key: {} {}", e.code, e.message);
                std::process::exit(1)
            }
        }
        return;
    }

    if let Some(Cmd::Token {
        action: TokenCmd::Sign { user, ttl },
    }) = &args.cmd
    {
        match hub.sign_token(user, *ttl) {
            Ok(t) => println!("{t}"),
            Err(e) => {
                eprintln!("token sign: {} {}", e.code, e.message);
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

    if let Some(Cmd::AdminKey { action }) = &args.cmd {
        let out = match action {
            AdminKeyCmd::Create { name, role } => {
                hub.create_admin_key(name, role).await.map(|(secret, row)| {
                    let mut v = row.view();
                    v["key"] = serde_json::json!(secret);
                    v["note"] = serde_json::json!(
                        "store the key now; it is not shown again. Use it with agentim admin --key"
                    );
                    v
                })
            }
            AdminKeyCmd::List => hub
                .admin_keys()
                .await
                .map(|k| serde_json::json!(k.iter().map(|k| k.view()).collect::<Vec<_>>())),
            AdminKeyCmd::Revoke { key_id } => hub
                .revoke_admin_key(key_id)
                .await
                .map(|_| serde_json::json!({"revoked": key_id})),
        };
        match out {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("admin-key: {} {}", e.code, e.message);
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
    let setup = hub.setup_token().await;
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
    if let Some(f) = crate::config::GENERATED_KEY.get() {
        println!(
            "Created the master key {f}: back it up with the database. Without it the encrypted content cannot be read."
        );
    }
    // No admin yet: a one-time link to set up the console (docs/12 §4).
    match setup {
        Ok(Some(t)) => {
            let host = if args.addr.ip().is_unspecified() {
                format!("127.0.0.1:{}", args.addr.port())
            } else {
                args.addr.to_string()
            };
            println!(
                "Set up the console (once, from this machine): {scheme}://{host}/console/setup?token={t}"
            );
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = e.message, "console setup link unavailable"),
    }
    if args.demo {
        println!("Demo mode: open the URL above and sign in as Alice, Bob or Carol.");
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
