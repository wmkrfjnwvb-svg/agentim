use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::serve::ListenerExt;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::edition::Edition;

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
    /// Encryption at rest (docs/04-security-compliance.md §2.6).
    Encryption {
        #[command(subcommand)]
        action: EncCmd,
    },
    /// Built-in accounts.
    User {
        #[command(subcommand)]
        action: UserCmd,
    },
    /// Open API keys (docs/07-open-api.md §5).
    ApiKey {
        #[command(subcommand)]
        action: KeyCmd,
    },
}

#[derive(Subcommand)]
enum EncCmd {
    /// Encrypts content stored before encryption was on. Run while the
    /// server is stopped; safe to repeat.
    EncryptExisting,
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
enum UserCmd {
    /// Sets a person's password, read from standard input, and logs out
    /// all of their devices. Creates the person when missing.
    SetPassword {
        #[arg(long)]
        name: String,
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
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

/// Runs the server or a command, as `agentim` or `agentim-lite`.
pub async fn run(edition: Edition) {
    let cmd = Args::command()
        .name(edition.binary())
        .about(format!("{} server", edition.product()));
    let args = Args::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());
    init_logging(&args.log_format);
    let cfg = match &args.config {
        Some(p) => Config::load(p).unwrap_or_else(|e| {
            eprintln!("config: {e}");
            std::process::exit(2)
        }),
        None => Config::single_sqlite(&args.db),
    };
    if let Some(Cmd::Encryption {
        action: EncCmd::GenerateKey,
    }) = &args.cmd
    {
        println!("{}", crate::crypto::Kek::generate_base64());
        return;
    }
    let hub_cfg = HubConfig {
        edition,
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

    if let Some(Cmd::Encryption { action }) = &args.cmd {
        let r = match action {
            EncCmd::GenerateKey => unreachable!("handled before opening"),
            EncCmd::EncryptExisting => hub.encrypt_existing().await,
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

    if let Some(Cmd::User {
        action: UserCmd::SetPassword { name },
    }) = &args.cmd
    {
        let mut pw = String::new();
        std::io::stdin().read_line(&mut pw).ok();
        let pw = pw.trim_end_matches(['\r', '\n']);
        let user = match hub.user_by_name(name) {
            Some(u) => Ok(u),
            None => hub
                .create_user(name, crate::model::UserKind::Human, None, None)
                .await
                .map(|(u, _)| u),
        };
        let r = match user {
            Ok(u) => hub.reset_password(&u, pw).await.map(|_| u),
            Err(e) => Err(e),
        };
        match r {
            Ok(u) => println!("password set for {} ({})", u.name, u.id),
            Err(e) => {
                eprintln!("user set-password: {} {}", e.code, e.message);
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
