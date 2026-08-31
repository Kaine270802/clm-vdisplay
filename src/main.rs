use clap::Parser;
use clm_vdisplay::config::{AppConfig, Cli, Commands};
use clm_vdisplay::metrics::{MetricsServer, GLOBAL_METRICS};
use clm_vdisplay::server::{DisplaySession, DisplaySupervisor};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

/// Wait for Unix multi-signal shutdown (SIGTERM, SIGINT, SIGHUP)
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("Failed to register SIGTERM handler: {}", e);
            None
        }
    };

    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("Failed to register SIGINT handler: {}", e);
            None
        }
    };

    let mut sighup = match signal(SignalKind::hangup()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("Failed to register SIGHUP handler: {}", e);
            None
        }
    };

    tokio::select! {
        _ = async {
            if let Some(ref mut s) = sigterm {
                s.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            info!("Received SIGTERM signal. Initiating graceful shutdown...");
        }
        _ = async {
            if let Some(ref mut s) = sigint {
                s.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            info!("Received SIGINT signal (Ctrl+C). Initiating graceful shutdown...");
        }
        _ = async {
            if let Some(ref mut s) = sighup {
                s.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            info!("Received SIGHUP signal. Initiating graceful shutdown...");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C interrupt. Initiating graceful shutdown...");
        }
    }
}

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
            manage_x11,
            attach,
            xvfb_path,
            xvfb_args,
            fps,
        } => {
            let app_config = AppConfig::from_start_args_full(
                &display,
                &resolution,
                rfb_port,
                ws_port,
                token.clone(),
                &mode,
                metrics_port,
                manage_x11,
                attach,
                xvfb_path,
                xvfb_args,
                fps,
            );

            info!(
                "Starting clm-vdisplay on display :{} (resolution={}x{}, mode={}, manage_x11={}, attach={}, fps={})",
                app_config.display_num, app_config.width, app_config.height, app_config.mode, app_config.manage_x11, app_config.attach, app_config.fps
            );

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

            let mut session = DisplaySession::from_app_config(&app_config);
            session.start().await?;

            println!(
                "clm-vdisplay initialized successfully on display :{}",
                app_config.display_num
            );

            // Wait for Unix termination signals (SIGTERM, SIGINT, SIGHUP)
            wait_for_shutdown_signal().await;

            info!("Stopping display session...");
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
                _ = wait_for_shutdown_signal() => {
                    info!("Stopping supervisor...");
                    cancel.cancel();
                }
            }

            info!("Supervisor daemon stopped cleanly.");
        }
    }

    Ok(())
}
