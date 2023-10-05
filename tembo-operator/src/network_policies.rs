use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::{
    api::{Patch, PatchParams},
    runtime::controller::Action,
    Api, Client,
};
use serde_json::Value;
use std::{net::IpAddr, time::Duration};
use tracing::{debug, error};
use trust_dns_resolver::Resolver;

pub async fn reconcile_network_policies(client: Client, namespace: &str) -> Result<(), Action> {
    let kubernetes_api_ip_addresses = lookup_kubernetes_api_ips()?;

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

    let mut kube_api_blocks = Vec::new();
    for ip in kubernetes_api_ip_addresses {
        let allowed_cidr = format!("{}/32", ip);
        let to_block_entry = serde_json::json!({
            "ipBlock": {
                "cidr": allowed_cidr,
            }
        });
        kube_api_blocks.push(to_block_entry);
    }

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
              "to": kube_api_blocks
            }
          ]
        }
    });
    apply_network_policy(namespace, &np_api, allow_kube_api).await?;

    Ok(())
}

fn lookup_kubernetes_api_ips() -> Result<Vec<String>, Action> {
    let resolver = match Resolver::default() {
        Ok(resolver) => resolver,
        Err(_) => {
            error!("Failed to create DNS resolver");
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    // Perform the DNS lookup
    let response = match resolver.lookup_ip("kubernetes.default.svc.cluster.local") {
        Ok(dns_lookup) => dns_lookup,
        Err(_) => {
            error!("Failed to DNS resolve kubernetes service address");
            return Err(Action::requeue(Duration::from_secs(300)));
        }
    };
    // Iterate through the IP addresses returned
    let mut results = vec![];
    for ip in response.iter() {
        match ip {
            IpAddr::V4(v4) => {
                results.push(v4.to_string());
                println!("IPv4 address: {}", v4);
            }
            IpAddr::V6(v6) => {
                error!("Found an IPv6 address while looking up kube API IP: {}", v6);
                return Err(Action::requeue(Duration::from_secs(300)));
            }
        }
    }
    // Deterministic ordering
    results.sort();
    if results.is_empty() {
        error!("Failed to find any IP addresses for kubernetes service");
        return Err(Action::requeue(Duration::from_secs(300)));
    }
    Ok(results)
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
