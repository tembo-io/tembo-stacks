use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub prometheus_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // The default value is the service name in kubernetes
            prometheus_url: from_env_default(
                "PROMETHEUS_URL",
                "http://monitoring-kube-prometheus-prometheus.monitoring.svc.cluster.local:9090",
            ),
        }
    }
}

/// source a variable from environment - use default if not exists
fn from_env_default(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}