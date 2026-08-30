use clap::Parser;
use clm_vdisplay::config::{AppConfig, Cli, Commands};
use clm_vdisplay::metrics::{MetricsServer, GLOBAL_METRICS};
use clm_vdisplay::server::{DisplaySession, DisplaySupervisor, SessionConfig};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            display,
            resolution,
            rfb_port,
            ws_port,
            token,
            mode,
            metrics_port,
        } => {
            let app_config = AppConfig::from_start_args(
                &display,
                &resolution,
                rfb_port,
                ws_port,
                token.clone(),
                &mode,
                metrics_port,
            );

            info!(
                "Starting clm-vdisplay on display :{} (resolution={}x{}, mode={})",
                app_config.display_num, app_config.width, app_config.height, app_config.mode
            );
            info!(
                "Listening on TCP RFB (vncviewer) port: 0.0.0.0:{}",
                app_config.rfb_port
            );
            if let Some(ws) = app_config.ws_port {
                info!("Listening on WebSocket (noVNC) port: 0.0.0.0:{}", ws);
            }

            let cancel_token = CancellationToken::new();

            // Optional Prometheus & Health HTTP probe
            if let Some(m_port) = app_config.metrics_port {
                let metrics_server =
                    MetricsServer::new(m_port, GLOBAL_METRICS.clone(), cancel_token.child_token());
                tokio::spawn(async move {
                    if let Err(e) = metrics_server.run().await {
                        error!("Metrics probe server exited with error: {}", e);
                    }
                });
            }

            let session_cfg = SessionConfig {
                display_num: app_config.display_num,
                width: app_config.width,
                height: app_config.height,
                rfb_port: app_config.rfb_port,
                ws_port: app_config.ws_port,
                auth_token: token,
                mode: app_config.mode,
            };

            let mut session = DisplaySession::new(session_cfg);
            session.start().await?;

            println!(
                "clm-vdisplay initialized successfully on display :{}",
                app_config.display_num
            );

            // Wait for termination signal (Ctrl+C, SIGTERM, SIGINT)
            tokio::signal::ctrl_c().await?;
            info!("Received shutdown signal. Stopping display session...");
            cancel_token.cancel();
            session.stop().await;
            info!("clm-vdisplay terminated cleanly.");
        }
        Commands::Daemon {
            base_vnc_port,
            control_socket,
            metrics_port,
        } => {
            info!(
                "Starting multi-display supervisor daemon (base_port={}, socket={})",
                base_vnc_port, control_socket
            );

            let supervisor = DisplaySupervisor::new(base_vnc_port, control_socket);
            let cancel = supervisor.cancel_token.clone();

            if let Some(m_port) = metrics_port {
                let metrics_server =
                    MetricsServer::new(m_port, GLOBAL_METRICS.clone(), cancel.child_token());
                tokio::spawn(async move {
                    if let Err(e) = metrics_server.run().await {
                        error!("Metrics probe server exited with error: {}", e);
                    }
                });
            }

            tokio::select! {
                res = supervisor.run_control_server() => {
                    if let Err(e) = res {
                        error!("Supervisor control server error: {}", e);
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Received shutdown signal. Stopping supervisor...");
                    cancel.cancel();
                }
            }

            info!("Supervisor daemon stopped cleanly.");
        }
    }

    Ok(())
}
