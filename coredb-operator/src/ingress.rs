use crate::ingress_route_tcp_crd::{
    IngressRouteTCP, IngressRouteTCPRoutes, IngressRouteTCPSpec, IngressRouteTCPTls,
};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Patch, PatchParams};
use kube::{Api, Client};
use serde_json::Value;
use tracing::log::{debug, error, info, warn};

fn postgres_ingress_route_tcp(
    name: String,
    namespace: String,
    matcher: String,
    service_name: String,
    port: IntOrString,
) -> IngressRouteTCP {
    return IngressRouteTCP {
        metadata: ingress_route_tcp.metadata,
        spec: IngressRouteTCPSpec {
            entry_points: ingress_route_tcp.spec.entry_points,
            routes: vec![IngressRouteTCPRoutes {
                r#match: matcher.clone(),
                services: Some(vec![IngressRouteTCPServices {
                    name: service_name_read_write.to_string(),
                    port,
                }]),
                ..Default::default()
            }],
            tls: Some(IngressRouteTCPTls {
                passthrough: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
}

// 1) We should never delete or update the hostname of an ingress route tcp.
//         Instead, just create another one if the hostname does not match.
//         This allows for domain name reconfiguration (e.g. coredb.io -> tembo.io),
//         with the old connection string still working.
// 2) We should replace the service and port target of all ingress route tcp
//         During a migration, the Service target will change, for example from CoreDB-operator managed
//         to CNPG managed read-write endpoints.
// 3) We should allow for additional ingress route tcp to be created for different use cases
//         For example read-only endpoints, we should not accidentally handle these other
//         IngressRouteTCP in this code, so we check that we are working with the correct type of Service.
pub async fn create_postgres_ing_route_tcp(
    client: Client,
    subdomain: &str,
    basedomain: &str,
    namespace: &str,
    service_name_read_write: &str,
    port: IntOrString,
) -> Result<(), ConductorError> {
    // There is only one per namespace, and we can name them as "postgres-rw-0"
    let ingress_route_tcp_name_prefix_rw = "postgres-rw-";

    // Initialize kube api for ingress route tcp
    let ing_api: Api<IngressRouteTCP> = Api::namespaced(client, namespace);

    // get all IngressRouteTCPs in the namespace
    let ingress_route_tcps = ing_api.list(&Default::default()).await?;

    // Build the expected IngressRouteTCP matcher we expect to find
    let matcher = format!("HostSNI(`{subdomain}.{basedomain}`)");

    // Check if any existing IngressRouteTCPs include the wrong service or port
    // All IngressRouteTCPs that have the right name prefix or named as the namespace (legacy naming)
    // should have the same service and port as the postgres read-write service.
    for ingress_route_tcp in ingress_route_tcps {
        // In this loop, we only are handling the read-write ingress route tcp
        let ingress_route_tcp_name = match ingress_route_tcp.metadata.name.clone() {
            Some(ingress_route_tcp_name) => {
                // The read-write ingress route tcp will have either the name as
                // the name of the namespace (if it was created by conductor) or
                // postgres-rw- (if it was created by this code).
                if !(ingress_route_tcp_name.starts_with(ingress_route_tcp_name_prefix_rw)
                    || ingress_route_tcp_name == namespace)
                {
                    debug!(
                        "Skipping non postgres-rw ingress route tcp: {}",
                        ingress_route_tcp_name
                    );
                    continue;
                }
                ingress_route_tcp_name
            }
            None => {
                error!(
                    "IngressRouteTCP {}.{}, does not have a name.",
                    subdomain, basedomain
                );
                return Err(ConductorError::IngressRouteTCPNameError);
            }
        };
        let service_name_actual = ingress_route_tcp.spec.routes[0].services[0].name;
        let service_port_actual = ingress_route_tcp.spec.routes[0].services[0].port;

        // Check if the service name and port match
        if !(service_name_actual == service_name_read_write && service_port_actual == port) {
            // This situation should only occur when the service name or port is changed, for example during cut-over from
            // CoreDB operator managing the service to CNPG managing the service.
            warn!(
                "Postgres read and write IngressRouteTCP {}, does not match the service name or port. Updating.",
                ingress_route_tcp_name
            );

            let patch = postgres_ingress_route_tcp(
                ingress_route_tcp_name,
                namespace.to_string(),
                matcher.clone(),
                service_name_read_write.to_string(),
                port.clone(),
            );
            // Update the service name and port of the IngressRouteTCP
            let patch = Patch::Merge(IngressRouteTCP {
                metadata: ingress_route_tcp.metadata,
                spec: IngressRouteTCPSpec {
                    entry_points: ingress_route_tcp.spec.entry_points,
                    routes: vec![IngressRouteTCPRoutes {
                        r#match: matcher.clone(),
                        services: Some(vec![IngressRouteTCPServices {
                            name: service_name_read_write.to_string(),
                            port,
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            });
            ing_api
                .patch(
                    &ingress_route_tcp.metadata.name.unwrap(),
                    &PatchParams::default(),
                    &patch,
                )
                .await?;
        }
        return Ok(());
    }

    // We do not already have an ingress route of the same name.
    let number_of_ingress_routes = ingress_route_tcps.len();
    let name = format!("ingress-route-tcp-{}", number_of_ingress_routes);

    // Check if there already is an IngressRouteTCP with the same name
    let ing_route_tcp = ing_api.get(name).await;
    if let Ok(ing_route_tcp) = ing_route_tcp {
        // If the hostname is the same, do nothing
        if ing_route_tcp.spec.routes[0].r#match == matcher {
            debug!(
                "IngressRouteTCP already exists for {}, doing nothing.",
                basedomain
            );
            return Ok(());
        }
        // If the hostname is different, create a new IngressRouteTCP
        else {
            info!("IngressRouteTCP already exists with name {}, but hostname is different. Creating new IngressRouteTCP", name);
            let new_name = format!("{}-{}", name);
        }
    }
    let params = PatchParams::apply("conductor").force();
    let ing = serde_json::json!({
        "apiVersion": "traefik.containo.us/v1alpha1",
        "kind": "IngressRouteTCP",
        "metadata": {
            "name": format!("{name}"),
            "namespace": format!("{name}"),
        },
        "spec": {
            "entryPoints": ["postgresql"],
            "routes": [
                {
                    "match": format!("HostSNI(`{name}.{basedomain}`)"),
                    "services": [
                        {
                            "name": format!("{name}"),
                            "port": 5432,
                        },
                    ],
                },
            ],
            "tls": {
                "passthrough": true,
            },
        },
    });
    info!("\nCreating or updating IngressRouteTCP: {}", name);
    let _o = ing_api
        .patch(name, &params, &Patch::Apply(&ing))
        .await
        .map_err(ConductorError::KubeError)?;
    Ok(())
}
