use k8s_openapi::api::{core::v1::Service, networking::v1::NetworkPolicy};
use kube::{
    api::{Patch, PatchParams},
    runtime::controller::Action,
    Api, Client,
};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, error};

pub async fn reconcile_network_policies(client: Client, namespace: &str) -> Result<(), Action> {
    let kubernetes_api_ip_address = lookup_kubernetes_api_ip(&client).await?;

    let np_api: Api<NetworkPolicy> = Api::namespaced(client, namespace);

    // Deny any network ingress or egress unless allowed
    // by another Network Policy
    let deny_all = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": format!("deny-all"),
            "namespace": format!("{namespace}"),
        },
        "spec": {
            "podSelector": {},
            "policyTypes": [
                "Egress",
                "Ingress"
            ],
        }
    });
    apply_network_policy(namespace, &np_api, deny_all).await?;

    let allow_dns = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
          "name": "allow-egress-to-kube-dns",
          "namespace": format!("{namespace}"),
        },
        "spec": {
          "podSelector": {},
          "policyTypes": [
            "Egress"
          ],
          "egress": [
            {
              "to": [
                {
                  "podSelector": {
                    "matchLabels": {
                      "k8s-app": "kube-dns"
                    }
                  },
                  "namespaceSelector": {
                    "matchLabels": {
                      "name": "kube-system"
                    }
                  }
                }
              ],
              "ports": [
                {
                  "protocol": "UDP",
                  "port": 53
                },
                {
                  "protocol": "TCP",
                  "port": 53
                }
              ]
            }
          ]
        }
    });
    apply_network_policy(namespace, &np_api, allow_dns).await?;

    let allow_system_ingress = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
          "name": "allow-system",
          "namespace": format!("{namespace}"),
        },
        "spec": {
          "podSelector": {},
          "policyTypes": ["Ingress"],
          "ingress": [
            {
              "from": [
                {
                  "namespaceSelector": {
                    "matchLabels": {
                      "name": "monitoring"
                    }
                  }
                },
                {
                  "namespaceSelector": {
                    "matchLabels": {
                      "name": "cnpg-system"
                    }
                  }
                },
                {
                  "namespaceSelector": {
                    "matchLabels": {
                      "name": "coredb-operator"
                    }
                  }
                },
                {
                  "namespaceSelector": {
                    "matchLabels": {
                      "name": "traefik"
                    }
                  }
                }
              ]
            }
          ]
        }
    });
    apply_network_policy(namespace, &np_api, allow_system_ingress).await?;

    let allow_public_internet = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
          "name": "allow-egress-to-internet",
          "namespace": format!("{namespace}"),
        },
        "spec": {
          "podSelector": {},
          "policyTypes": ["Egress"],
          "egress": [
            {
              "to": [
                {
                  "ipBlock": {
                    "cidr": "0.0.0.0/0",
                    "except": [
                      "10.0.0.0/8",
                      "172.16.0.0/12",
                      "192.168.0.0/16"
                    ]
                  }
                }
              ]
            }
          ]
        }
    });
    apply_network_policy(namespace, &np_api, allow_public_internet).await?;

    let allow_within_namespace = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
          "name": "allow-within-namespace",
          "namespace": format!("{namespace}"),
        },
        "spec": {
          "podSelector": {},
          "policyTypes": ["Ingress", "Egress"],
          "ingress": [
            {
              "from": [
                {
                  "podSelector": {}
                }
              ]
            }
          ],
          "egress": [
            {
              "to": [
                {
                  "podSelector": {}
                }
              ]
            }
          ]
        }
    });
    apply_network_policy(namespace, &np_api, allow_within_namespace).await?;

    let allow_kube_api = serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
          "name": "allow-kube-api",
          "namespace": format!("{namespace}"),
        },
        "spec": {
          "podSelector": {},
          "policyTypes": ["Egress"],
          "egress": [
            {
              "to": [
                {
                  "ipBlock": {
                    "cidr": format!("{}/32", kubernetes_api_ip_address)
                  }
                }
              ]
            }
          ]
        }
    });
    apply_network_policy(namespace, &np_api, allow_kube_api).await?;

    Ok(())
}

async fn lookup_kubernetes_api_ip(client: &Client) -> Result<String, Action> {
    let service_api = Api::<Service>::namespaced(client.clone(), "default");
    // Look up IP address of 'kubernetes' service in default namespace
    let kubernetes_service = match service_api.get("kubernetes").await {
        Ok(s) => s,
        Err(_) => {
            error!("Failed to get kubernetes service");
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    let kubernetes_service_spec = match kubernetes_service.spec {
        Some(s) => s,
        None => {
            error!("while discovering kubernetes API IP address, service has no spec");
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    let cluster_ip = match kubernetes_service_spec.cluster_ip.clone() {
        Some(c) => c,
        None => {
            error!("while discovering kubernetes API IP address, service has no cluster IP");
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    Ok(cluster_ip)
}

async fn apply_network_policy(namespace: &str, np_api: &Api<NetworkPolicy>, np: Value) -> Result<(), Action> {
    let network_policy: NetworkPolicy = match serde_json::from_value(np) {
        Ok(np) => np,
        Err(_) => {
            error!("Failed to deserialize Network Policy namespace {}", namespace);
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    let name = network_policy
        .metadata
        .name
        .clone()
        .expect("There is always a name for a network policy")
        .clone();
    let params: PatchParams = PatchParams::apply("conductor").force();
    debug!("\nApplying Network Policy {} in namespace {}", name, namespace);
    let _o: NetworkPolicy = match np_api.patch(&name, &params, &Patch::Apply(&network_policy)).await {
        Ok(np) => np,
        Err(_) => {
            error!(
                "Failed to create Network Policy {} in namespace {}",
                name, namespace
            );
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    Ok(())
}
