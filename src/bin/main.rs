use std::future::ready;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::extract::{MatchedPath, Request};
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use clap::{Parser, Subcommand, ValueEnum};
use console::{style, Term};
use fedimint_core::invite_code::InviteCode;
use fmcd::api::rest::{admin, ln, mint, onchain};
use fmcd::api::websockets::websocket_handler;
use fmcd::auth::{basic_auth_middleware, BasicAuth, WebSocketAuth};
use fmcd::config::Config;
use fmcd::core::FmcdCore;
use fmcd::health::{health_check, liveness_check, readiness_check};
use fmcd::metrics::{api_metrics, init_prometheus_metrics};
use fmcd::observability::correlation::create_request_id_middleware;
use fmcd::observability::{init_logging, LoggingConfig};
use fmcd::state::AppState;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone, Debug, ValueEnum, PartialEq)]
enum Mode {
    Rest,
    Ws,
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rest" => Ok(Mode::Rest),
            "ws" => Ok(Mode::Ws),
            _ => Err(anyhow::anyhow!("Invalid mode")),
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    Start,
    Stop,
}

#[derive(Parser)]
#[clap(version = "1.0")]
struct Cli {
    /// Data directory path (contains config and database)
    #[clap(long, env = "FMCD_DATA_DIR", default_value = ".")]
    data_dir: PathBuf,

    /// Federation invite code (overrides config)
    #[clap(long, env = "FMCD_INVITE_CODE")]
    invite_code: Option<String>,

    /// Password (overrides config)
    #[clap(long, env = "FMCD_PASSWORD")]
    password: Option<String>,

    /// Server address (overrides config)
    #[clap(long, env = "FMCD_ADDR")]
    addr: Option<String>,

    /// Manual secret (overrides config)
    #[clap(long, env = "FMCD_MANUAL_SECRET")]
    manual_secret: Option<String>,

    /// Mode: ws, rest
    #[clap(long, env = "FMCD_MODE", default_value = "rest")]
    mode: Mode,

    /// Disable authentication
    #[clap(long)]
    no_auth: bool,
}

// const PID_FILE: &str = "/tmp/fedimint_http.pid";

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    fedimint_core::rustls::install_crypto_provider().await;

    let cli: Cli = Cli::parse();

    // Initialize structured logging
    let log_config = LoggingConfig {
        level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
        log_dir: cli.data_dir.join("logs"),
        console_output: std::env::var("NO_CONSOLE_LOG").is_err(),
        file_output: std::env::var("NO_FILE_LOG").is_err(),
        ..Default::default()
    };
    init_logging(log_config)?;

    tracing::info!("Starting FMCD with structured logging and observability");

    // Ensure data directory exists
    std::fs::create_dir_all(&cli.data_dir)?;

    // Config file is always in data_dir
    let config_path = cli.data_dir.join("fmcd.conf");

    // Load or create configuration file with automatic password generation
    let term = Term::stdout();
    let (mut config, password_generated) = Config::load_or_create(&config_path)?;

    if password_generated {
        term.write_line(&format!(
            "{}{}",
            style("Generating default api password...").yellow(),
            style("done").white()
        ))?;
    }

    // Override config with CLI arguments
    if let Some(invite_code) = cli.invite_code {
        config.invite_code = Some(invite_code);
    }
    // Update config's data_dir to match CLI
    config.data_dir = Some(cli.data_dir.clone());
    if let Some(password) = cli.password {
        config.http_password = Some(password);
    }
    if let Some(addr) = cli.addr {
        // Parse address to extract IP and port
        if let Some((ip, port_str)) = addr.split_once(':') {
            config.http_bind_ip = ip.to_string();
            if let Ok(port) = port_str.parse::<u16>() {
                config.http_bind_port = port;
            }
        }
    }
    if let Some(manual_secret) = cli.manual_secret {
        config.manual_secret = Some(manual_secret);
    }
    if cli.no_auth {
        config.http_password = None;
    }

    // Initialize FmcdCore with the data directory
    let core = FmcdCore::new_with_config(cli.data_dir.clone(), config.webhooks.clone()).await?;

    // Handle federation invite code
    if let Some(invite_code_str) = &config.invite_code {
        match InviteCode::from_str(invite_code_str) {
            Ok(invite_code) => {
                let federation_id = core.join_federation(invite_code, None).await?;
                info!("Created client for federation id: {:?}", federation_id);
            }
            Err(e) => {
                info!(
                    "No federation invite code provided, skipping client creation: {}",
                    e
                );
            }
        }
    }

    if core.multimint.all().await.is_empty() {
        return Err(anyhow::anyhow!("No clients found, must have at least one client to start the server. Try providing a federation invite code with the `--invite-code` flag or setting the `FMCD_INVITE_CODE` environment variable."));
    }

    // Start monitoring services for full observability parity
    if let Err(e) = core.start_monitoring_services().await {
        tracing::warn!("Failed to start monitoring services: {}", e);
    } else {
        tracing::info!("Monitoring services started successfully");
    }

    // Create AppState with the core
    let state = AppState::new_with_core(core).await?;

    start_main_server(&config, cli.mode, state).await?;
    Ok(())
}

async fn start_main_server(config: &Config, mode: Mode, state: AppState) -> anyhow::Result<()> {
    // Create authentication instances
    let basic_auth = Arc::new(BasicAuth::new(config.http_password.clone()));
    let ws_auth = Arc::new(WebSocketAuth::new(config.http_password.clone()));

    // Create the router based on mode
    let app = match mode {
        Mode::Rest => {
            let router = Router::new()
                .nest("/v2", fedimint_v2_rest())
                .with_state(state);

            // Apply authentication middleware if enabled
            if basic_auth.is_enabled() {
                let auth_clone = basic_auth.clone();
                router.route_layer(middleware::from_fn(move |request, next| {
                    basic_auth_middleware(auth_clone.clone(), request, next)
                }))
            } else {
                router
            }
        }
        Mode::Ws => Router::new()
            .route("/ws", get(websocket_handler))
            .with_state(state.clone())
            .layer(axum::Extension(ws_auth)),
    };

    let auth_status = if config.is_auth_enabled() {
        "enabled"
    } else {
        "disabled"
    };
    info!("Starting server in {mode:?} mode with authentication {auth_status}");

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin(Any)
        .allow_headers(Any);

    // Initialize comprehensive metrics system
    let metrics_handle = init_prometheus_metrics().await?;

    let app = app
        .layer(middleware::from_fn(create_request_id_middleware(
            config.rate_limiting.clone(),
        )))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .route("/health", get(health_check))
        .route("/health/live", get(liveness_check))
        .route("/health/ready", get(readiness_check))
        .route("/metrics", get(move || ready(metrics_handle.render())))
        .route_layer(middleware::from_fn(track_metrics));

    let addr = config.http_address();
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("fmcd listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn track_metrics(req: Request, next: Next) -> impl IntoResponse {
    let start = Instant::now();
    let path = if let Some(matched_path) = req.extensions().get::<MatchedPath>() {
        matched_path.as_str().to_owned()
    } else {
        req.uri().path().to_owned()
    };
    let method = req.method().clone();

    let response = next.run(req).await;

    let duration = start.elapsed();
    let status_code = response.status().as_u16();

    // Use our comprehensive API metrics recording
    api_metrics::record_api_request(method.as_ref(), &path, status_code, duration);

    response
}

/// Implements Fedimint V0.2 API Route matching against CLI commands:
/// - `/v2/admin/backup`: Upload the (encrypted) snapshot of mint notes to
///   federation.
/// - `/v2/admin/version`: Discover the common api version to use to communicate
///   with the federation.
/// - `/v2/admin/info`: Display wallet info (holdings, tiers).
/// - `/v2/admin/join`: Join a federation with an invite code.
/// - `/v2/admin/restore`: Restore the previously created backup of mint notes
///   (with `backup` command).
/// - `/v2/admin/operations`: List operations.
/// - `/v2/admin/module`: Call a module subcommand.
/// - `/v2/admin/config`: Returns the client config.
///
/// Mint related commands:
/// - `/v2/mint/reissue`: Reissue notes received from a third party to avoid
///   double spends.
/// - `/v2/mint/spend`: Prepare notes to send to a third party as a payment.
/// - `/v2/mint/validate`: Verifies the signatures of e-cash notes, but *not* if
///   they have been spent already.
/// - `/v2/mint/split`: Splits a string containing multiple e-cash notes (e.g.
///   from the `spend` command) into ones that contain exactly one.
/// - `/v2/mint/combine`: Combines two or more serialized e-cash notes strings.
///
/// Lightning network related commands:
/// - `/v2/ln/invoice`: Create a lightning invoice to receive payment via
///   gateway.
/// - `/v2/ln/pay`: Pay a lightning invoice or lnurl via a gateway.
/// - `/v2/ln/gateways`: List registered gateways.
///
/// Onchain related commands:
/// - `/v2/onchain/deposit-address`: Generate a new deposit address, funds sent
///   to it can later be claimed.
/// - `/v2/onchain/await-deposit`: Wait for deposit on previously generated
///   address.
/// - `/v2/onchain/withdraw`: Withdraw funds from the federation.
fn fedimint_v2_rest() -> Router<AppState> {
    let mint_router = Router::new()
        .route("/decode-notes", post(mint::decode_notes::handle_rest))
        .route("/encode-notes", post(mint::encode_notes::handle_rest))
        .route("/reissue", post(mint::reissue::handle_rest))
        .route("/spend", post(mint::spend::handle_rest))
        .route("/validate", post(mint::validate::handle_rest))
        .route("/split", post(mint::split::handle_rest))
        .route("/combine", post(mint::combine::handle_rest));

    let ln_router = Router::new()
        // Modern API endpoints - aligns with fedimint client 0.8 behavior
        .route("/invoice", post(ln::invoice::handle_rest))
        .route("/invoice/status/bulk", post(ln::status::handle_bulk_status))
        .route(
            "/operation/:operation_id/status",
            get(ln::status::handle_rest_by_operation_id),
        )
        .route(
            "/operation/:operation_id/stream",
            get(ln::stream::handle_operation_stream),
        )
        .route(
            "/events/stream",
            get(ln::stream::handle_global_event_stream),
        )
        // Other LN endpoints
        .route("/pay", post(ln::pay::handle_rest))
        .route("/gateways", post(ln::gateways::handle_rest));

    let onchain_router = Router::new()
        .route(
            "/deposit-address",
            post(onchain::deposit_address::handle_rest),
        )
        .route("/await-deposit", post(onchain::await_deposit::handle_rest))
        .route("/withdraw", post(onchain::withdraw::handle_rest));

    let admin_router = Router::new()
        .route("/backup", post(admin::backup::handle_rest))
        .route("/version", post(admin::version::handle_rest))
        .route("/federations", get(admin::federations::handle_rest))
        .route("/info", get(admin::info::handle_rest))
        .route("/join", post(admin::join::handle_rest))
        .route("/restore", post(admin::restore::handle_rest))
        // .route("/printsecret", get(handle_printsecret)) TODO: should I expose this
        // under admin?
        .route("/operations", post(admin::operations::handle_rest))
        .route(
            "/operations/tracked",
            post(admin::operations::handle_tracked_rest),
        )
        .route("/module", post(admin::module::handle_rest))
        .route("/config", get(admin::config::handle_rest));

    Router::new()
        .nest("/admin", admin_router)
        .nest("/mint", mint_router)
        .nest("/ln", ln_router)
        .nest("/onchain", onchain_router)
}
