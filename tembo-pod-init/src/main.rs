use actix_web::{dev::ServerHandle, web, App, HttpServer};
use kube::Client;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};
use opentelemetry::global;
use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use tembo_pod_init::{
    config::Config,
    health::*,
    mutate::mutate,
    watcher::{CertificateUpdateHandler, NamespaceWatcher},
};
use tembo_telemetry::{TelemetryConfig, TelemetryInit};
use tracing::*;

const TRACER_NAME: &str = "tembo.io/tembo-pod-init";

#[instrument(fields(trace_id))]
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config = Config::default();

    // Initialize logging
    let otlp_endpoint_url = &config.opentelemetry_endpoint_url;
    let telemetry_config = TelemetryConfig {
        app_name: "tembo-pod-init".to_string(),
        env: std::env::var("ENV").unwrap_or_else(|_| "production".to_string()),
        endpoint_url: otlp_endpoint_url.clone(),
        tracer_id: Some(TRACER_NAME.to_string()),
    };

    let _ = TelemetryInit::init(&telemetry_config).await;

    // Set trace_id for logging
    let trace_id = telemetry_config.get_trace_id();
    Span::current().record("trace_id", &field::display(&trace_id));

    let stop_handle = web::Data::new(StopHandle::default());

    // Setup Kubernetes Client
    let kube_client = match Client::try_default().await {
        Ok(client) => client,
        Err(e) => {
            panic!("Failed to create Kubernetes client: {}", e);
        }
    };

    // Start watching namespaces in a seperate tokio task thread
    let ns_watcher = NamespaceWatcher::new(Arc::new(kube_client.clone()), config.clone());
    let namespaces = ns_watcher.get_namespaces();
    tokio::spawn(watch_namespaces(ns_watcher));

    // Set up a channel to receive filesystem events
    // This is a way to reset the SslAcceptor when the certificate changes
    // and allow a reload of the webserver to use the new certificate
    let should_restart = Arc::new(AtomicBool::new(false));
    let cert_path = config.tls_cert.clone();
    let key_path = config.tls_key.clone();

    // Clone for the CertificateUpdateHandler
    let event_handler_should_restart = should_restart.clone();
    let event_handler = CertificateUpdateHandler {
        should_restart: event_handler_should_restart,
    };

    // Clone for usage inside the async block
    let should_restart_clone = should_restart.clone();

    tokio::spawn(async move {
        let (_tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher: RecommendedWatcher =
            match notify::Watcher::new(event_handler, notify::Config::default()) {
                Ok(w) => w,
                Err(e) => {
                    error!("Error creating filesystem watcher: {}", e);
                    return;
                }
            };
        if let Err(e) = watcher.watch(cert_path.as_ref(), RecursiveMode::NonRecursive) {
            error!("Error watching TLS certificate: {}", e);
            return;
        }
        if let Err(e) = watcher.watch(key_path.as_ref(), RecursiveMode::NonRecursive) {
            error!("Error watching TLS key: {}", e);
            return;
        }

        loop {
            async_std::task::block_on(async {
                match rx.recv() {
                    Ok(_) => {
                        // When a change is detected, set the should_restart flag
                        info!("TLS certificate changed, restarting server.");
                        should_restart_clone.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        error!("Error watching TLS certificate: {}", e);
                    }
                }
            });
        }
    });

    loop {
        // Reload the TLS certificate and key file
        let mut tls_config = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
        tls_config
            .set_private_key_file(config.tls_key.clone(), SslFiletype::PEM)
            .unwrap();
        tls_config
            .set_certificate_chain_file(config.tls_cert.clone())
            .unwrap();
        let server_bind_address = format!("{}:{}", config.server_host, config.server_port);

        let server = HttpServer::new({
            let config_data = web::Data::new(config.clone());
            let kube_data = web::Data::new(Arc::new(kube_client.clone()));
            let namespace_watcher_data = web::Data::new(namespaces.clone());
            let stop_handle = stop_handle.clone();
            let tc = web::Data::new(telemetry_config.clone());
            move || {
                {
                    App::new()
                        .app_data(config_data.clone())
                        .app_data(kube_data.clone())
                        .app_data(namespace_watcher_data.clone())
                        .app_data(stop_handle.clone())
                        .app_data(tc.clone())
                        .wrap(
                            tembo_telemetry::get_tracing_logger()
                                .exclude("/health/liveness")
                                .exclude("/health/readiness")
                                .build(),
                        )
                        .service(liveness)
                        .service(readiness)
                        .service(mutate)
                }
            }
        })
        .bind_openssl(server_bind_address, tls_config)?
        .shutdown_timeout(5)
        .run();

        stop_handle.register(server.handle());

        info!(
            "Starting HTTPS server at https://{}:{}/",
            config.server_host, config.server_port
        );
        debug!("Config: {:?}", config);
        server.await?;

        // Make sure we close all the spans
        global::shutdown_tracer_provider();

        // If the certificate hasn't changed, break out of the loop.
        if !should_restart.load(Ordering::Relaxed) {
            break;
        }

        // Reset the flag for the next iteration
        should_restart.store(false, Ordering::Relaxed);
    }

    Ok(())
}

#[derive(Default)]
struct StopHandle {
    inner: Mutex<Option<ServerHandle>>,
}

impl StopHandle {
    // Set the ServerHandle to stop
    pub(crate) fn register(&self, handle: ServerHandle) {
        *self.inner.lock() = Some(handle);
    }
}

#[instrument(skip(watcher))]
async fn watch_namespaces(watcher: NamespaceWatcher) {
    loop {
        match watcher.watch().await {
            Ok(_) => {
                info!("Namespace watcher finished, restarting.");
            }
            Err(e) => {
                error!("Namespace watcher failed, restarting: {}", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn test_certificate_update_handling() {
        // Create a temporary directory
        let dir = tempdir().unwrap();

        // Create a temporary certificate file
        let cert_path = dir.path().join("test_cert.pem");
        let mut file = File::create(&cert_path).unwrap();
        writeln!(file, "test certificate content").unwrap();

        // Set up the watcher and the restart flag
        let should_restart = Arc::new(AtomicBool::new(false));
        let event_handler = CertificateUpdateHandler {
            should_restart: should_restart.clone(),
        };

        let mut watcher: RecommendedWatcher =
            notify::Watcher::new(event_handler, notify::Config::default()).unwrap();
        watcher
            .watch(cert_path.as_path(), RecursiveMode::NonRecursive)
            .unwrap();

        // Update the certificate file after a short delay
        let update_path = cert_path.clone();
        std::thread::spawn(move || {
            sleep(Duration::from_secs(1));
            let mut file = File::create(update_path).unwrap();
            writeln!(file, "updated certificate content").unwrap();
        });

        // Sleep for some time to allow the watcher to detect the update
        sleep(Duration::from_secs(2));

        // Ensure that the `should_restart` flag is set to true
        assert!(should_restart.load(Ordering::Relaxed));
    }
}
