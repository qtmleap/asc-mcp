use anyhow::{Result, bail};
use asc_mcp::{
    catalog, http,
    protocol::{Server, Session},
    reviewer::{self, Reviewer},
};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
#[derive(Parser)]
#[command(version, about = "Rust App Store Connect MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    ServeHttp {
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        #[arg(long, default_value = "")]
        allowed_origins: String,
        #[arg(long, env = "ASC_MCP_AUTH_TOKENS", default_value = "")]
        auth_tokens: String,
        #[arg(long)]
        tls_cert: Option<String>,
        #[arg(long)]
        tls_key: Option<String>,
        #[arg(long, default_value = "json")]
        log_format: String,
        #[arg(long, default_value = "info")]
        log_level: String,
    },
    Tools,
    Validate,
    Version,
    ReviewerCheck {
        #[arg(long)]
        ack: Option<String>,
    },
    ReviewerImportSession {
        source: PathBuf,
    },
    ReviewerRenew,
    ReviewerLogin {
        #[arg(long)]
        apple_id: Option<String>,
        /// Skip session preflight for recovery; still reuse private trust cookies.
        #[arg(long)]
        fresh: bool,
        /// Explicitly accept a different App Store Connect team after human login.
        #[arg(long)]
        accept_provider_change: bool,
    },
}
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Serve => stdio(Arc::new(Server::from_env()?)).await,
        Command::ServeHttp {
            addr,
            allowed_origins,
            auth_tokens,
            tls_cert,
            tls_key,
            log_format,
            log_level,
        } => {
            if !["json", "text"].contains(&log_format.as_str())
                || !["debug", "info", "warn", "error"].contains(&log_level.as_str())
            {
                bail!("invalid log format/level")
            }
            let options = http::Options {
                allowed_origins: csv(&allowed_origins),
                auth_tokens: csv(&auth_tokens),
            };
            match (tls_cert, tls_key) {
                (Some(cert), Some(key)) => {
                    http::serve_tls(Arc::new(Server::from_env()?), &addr, options, &cert, &key)
                        .await
                }
                (None, None) => http::serve(Arc::new(Server::from_env()?), &addr, options).await,
                _ => bail!("--tls-cert and --tls-key must be provided together"),
            }
        }
        Command::Tools => {
            let mut tools = catalog::tools();
            if let Some(r) = Reviewer::from_env()? {
                tools.extend(r.tools())
            };
            println!("{}", serde_json::to_string_pretty(&tools)?);
            Ok(())
        }
        Command::Validate => {
            let s = Server::from_env()?;
            if s.api.is_none() && s.reviewer.is_none() {
                bail!("configure public API credentials or reviewer session")
            };
            println!("configuration valid (live authentication not tested)");
            Ok(())
        }
        Command::Version => {
            println!("asc-mcp {} (Rust)", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::ReviewerCheck { ack } => {
            let mut monitor = Reviewer::from_env()?;
            if monitor.is_none() {
                // Explicit command opts in even when no session exists; the check reports
                // authentication_required, rather than disguising missing auth as no news.
                // Native reviewer constructor supplies this helper without modifying env.
                monitor = Some(Reviewer::new_default()?);
            }
            let r = monitor.as_ref().unwrap();
            let output = if let Some(token) = ack {
                r.acknowledge(&token)?
            } else {
                match r.check_updates().await {
                    Ok(v) => v,
                    Err(e) => json!({"status":"blocked","reason":e.to_string()}),
                }
            };
            println!("{}", serde_json::to_string(&output)?);
            if output["status"] == "blocked" {
                std::process::exit(2)
            }
            Ok(())
        }
        Command::ReviewerRenew => {
            println!("{}", asc_mcp::renewal::run_once().await?);
            Ok(())
        }
        Command::ReviewerLogin {
            apple_id,
            fresh,
            accept_provider_change,
        } => {
            use std::io::{IsTerminal, Write};
            if !std::io::stdin().is_terminal() {
                bail!(
                    "reviewer-login requires an interactive terminal; scheduled checks never request passwords or 2FA"
                )
            }
            let email =
                if let Some(email) = asc_mcp::apple_login::resolve_account(apple_id.as_deref())? {
                    email
                } else {
                    print!("Apple ID: ");
                    std::io::stdout().flush()?;
                    let mut value = String::new();
                    std::io::stdin().read_line(&mut value)?;
                    value.trim().to_owned()
                };
            let path =
                asc_mcp::renewal::interactive_login(&email, fresh, accept_provider_change).await?;
            println!("Session saved privately to {}", path.display());
            Ok(())
        }
        Command::ReviewerImportSession { source } => {
            let path = reviewer::import_session(&source)?;
            println!("Session imported privately to {}", path.display());
            Ok(())
        }
    }
}
fn csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
async fn stdio(server: Arc<Server>) -> Result<()> {
    let session = Arc::new(Session::default());
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let pending: Arc<Mutex<HashMap<String, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let semaphore = Arc::new(tokio::sync::Semaphore::new(16));
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        if line.len() > 64 << 20 {
            bail!("MCP request exceeds size limit")
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                write_response(
                    &stdout,
                    asc_mcp::protocol::error(Value::Null, -32700, "Parse error"),
                )
                .await?;
                continue;
            }
        };
        if request["method"] == "notifications/cancelled" {
            if let Some(id) = request.pointer("/params/requestId") {
                let key = id.to_string();
                if let Some(token) = pending.lock().await.get(&key) {
                    token.cancel()
                }
            }
            continue;
        }
        if request["method"] != "tools/call" {
            if let Some(response) = server.dispatch(&request, &session).await {
                write_response(&stdout, response).await?
            }
            continue;
        }
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                write_response(
                    &stdout,
                    asc_mcp::protocol::error(
                        request.get("id").cloned().unwrap_or(Value::Null),
                        -32000,
                        "Too many concurrent tool calls",
                    ),
                )
                .await?;
                continue;
            }
        };
        let Some(id) = request.get("id") else {
            continue;
        };
        let key = id.to_string();
        let token = CancellationToken::new();
        {
            let mut pending = pending.lock().await;
            if pending.contains_key(&key) {
                write_response(
                    &stdout,
                    asc_mcp::protocol::error(id.clone(), -32600, "Duplicate in-flight request ID"),
                )
                .await?;
                continue;
            };
            pending.insert(key.clone(), token.clone());
        }
        let server = server.clone();
        let session = session.clone();
        let stdout = stdout.clone();
        let pending = pending.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let response = tokio::select! {_ = token.cancelled()=>None,result=server.dispatch(&request,&session)=>result};
            if let Some(value) = response {
                let _ = write_response(&stdout, value).await;
            }
            pending.lock().await.remove(&key);
        });
    }
    // EOF drains work already accepted, matching stdio clients that close stdin
    // after a finite request batch; cancellation notifications remain available.
    let _drain = semaphore.acquire_many(16).await?;
    Ok(())
}
async fn write_response(stdout: &Arc<Mutex<tokio::io::Stdout>>, value: Value) -> Result<()> {
    let mut out = stdout.lock().await;
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    out.write_all(&bytes).await?;
    out.flush().await?;
    Ok(())
}
