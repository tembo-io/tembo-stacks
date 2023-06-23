use crate::ingress_route_tcp_crd::{
    IngressRouteTCP, IngressRouteTCPRoutes, IngressRouteTCPRoutesServices, IngressRouteTCPSpec,
    IngressRouteTCPTls,
};
use k8s_openapi::apimachinery::pkg::{
    apis::meta::v1::{ObjectMeta, OwnerReference},
    util::intstr::IntOrString,
};
use kube::{
    api::{Patch, PatchParams},
    Api, Client,
};
use std::collections::BTreeMap;

use crate::errors::OperatorError;
use tracing::log::{debug, error, info, warn};

fn postgres_ingress_route_tcp(
    name: String,
    namespace: String,
    owner_reference: OwnerReference,
    labels: BTreeMap<String, String>,
    matcher: String,
    service_name: String,
    port: IntOrString,
) -> IngressRouteTCP {
    IngressRouteTCP {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(namespace),
            labels: Some(labels),
            owner_references: Some(vec![owner_reference]),
            ..ObjectMeta::default()
        },
        spec: IngressRouteTCPSpec {
            entry_points: Some(vec!["postgresql".to_string()]),
            routes: vec![IngressRouteTCPRoutes {
                r#match: matcher,
                services: Some(vec![IngressRouteTCPRoutesServices {
                    name: service_name,
                    port,
                    // I'm not sure how to just use defaults, should look like this:
                    // ..IngressRouteTCPRoutesServices::default()
                    namespace: None,
                    proxy_protocol: None,
                    termination_delay: None,
                    weight: None,
                }]),
                middlewares: None,
                priority: None,
            }],
            tls: Some(IngressRouteTCPTls {
                passthrough: Some(true),
                cert_resolver: None,
                domains: None,
                options: None,
                secret_name: None,
                store: None,
            }),
        },
    }
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
    owner_reference: OwnerReference,
    labels: BTreeMap<String, String>,
    subdomain: &str,
    basedomain: &str,
    namespace: &str,
    service_name_read_write: &str,
    port: IntOrString,
) -> Result<(), OperatorError> {
    // Initialize kube api for ingress route tcp
    let ingress_route_tcp_api: Api<IngressRouteTCP> = Api::namespaced(client, namespace);

    // get all IngressRouteTCPs in the namespace
    // After CNPG migration is done, this can look for only ingress route tcp with the correct owner reference
    let ingress_route_tcps = ingress_route_tcp_api.list(&Default::default()).await?;

    let ingress_route_tcp_name_prefix_rw = "postgres-rw-";

    // We will save information about the existing ingress route tcp(s) in these vectors
    let mut present_matchers_list: Vec<String> = vec![];
    let mut present_ing_route_tcp_names_list: Vec<String> = vec![];

    // Check for all existing IngressRouteTCPs in this namespace
    for ingress_route_tcp in ingress_route_tcps {
        // Get the name
        let ingress_route_tcp_name = match ingress_route_tcp.metadata.name.clone() {
            Some(ingress_route_tcp_name) => {
                // We only are handling the read-write endpoint:
                //        The read-write ingress route tcp will have either the same name as
                //        namespace (if it was created by conductor) or postgres-rw-
                //        (if it was created by this code).
                if !(ingress_route_tcp_name.starts_with(ingress_route_tcp_name_prefix_rw)
                    || ingress_route_tcp_name == namespace)
                {
                    // Skipping unexpected ingress route TCP is important for allowing
                    // manual creation of other ingress route TCPs, and other use cases
                    // like read only endpoints.
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
                return Err(OperatorError::IngressRouteTCPNameError);
            }
        };
        debug!(
            "Detected ingress route tcp read write endpoint {}.{}",
            ingress_route_tcp_name, namespace
        );
        // Save list of names so we can pick a name that doesn't exist,
        // if we need to create a new ingress route tcp.
        present_ing_route_tcp_names_list.push(ingress_route_tcp_name.clone());

        // Get the settings of our ingress route tcp, so we can update to a new
        // endpoint, if needed.

        // TODO: before merge, clean this up and error handle
        let service_name_actual = ingress_route_tcp.spec.routes[0].services.clone().unwrap()[0]
            .name
            .clone();
        let service_port_actual = ingress_route_tcp.spec.routes[0].services.clone().unwrap()[0]
            .port
            .clone();

        // Keep the existing matcher (domain name) when updating an existing IngressRouteTCP,
        // so that we do not break connection strings with domain name updates.
        let matcher_actual = ingress_route_tcp.spec.routes[0].r#match.clone();

        // Save the matchers to know if we need to create a new ingress route tcp or not.
        present_matchers_list.push(matcher_actual.clone());

        // Check if either the service name or port are mismatched
        if !(service_name_actual == service_name_read_write && service_port_actual == port) {
            // This situation should only occur when the service name or port is changed, for example during cut-over from
            // CoreDB operator managing the service to CNPG managing the service.
            warn!(
                "Postgres read and write IngressRouteTCP {}.{}, does not match the service name or port. Updating service or port and leaving the match rule the same.",
                ingress_route_tcp_name, namespace
            );

            // We will keep the matcher and the name the same, but update the service name and port.
            // Also, we will set ownership and labels.
            let ingress_route_tcp_to_apply = postgres_ingress_route_tcp(
                ingress_route_tcp_name.clone(),
                namespace.to_string(),
                owner_reference.clone(),
                labels.clone(),
                matcher_actual.clone(),
                service_name_read_write.to_string(),
                port.clone(),
            );
            // Apply this ingress route tcp
            let patch = Patch::Apply(&ingress_route_tcp_to_apply);
            let patch_parameters = PatchParams::apply("cntrlr").force();
            match ingress_route_tcp_api
                .patch(&ingress_route_tcp_name.clone(), &patch_parameters, &patch)
                .await
            {
                Ok(_) => {
                    info!(
                        "Updated postgres read and write IngressRouteTCP {}.{}",
                        ingress_route_tcp_name.clone(),
                        namespace
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to update postgres read and write IngressRouteTCP {}.{}: {}",
                        ingress_route_tcp_name, namespace, e
                    );
                    return Err(OperatorError::IngressRouteTCPUpdateError);
                }
            }
        }
    }

    // At this point in the code, all applicable IngressRouteTCPs are pointing to the right
    // service and port. Now, we just need to create a new IngressRouteTCP if we do not already
    // have one for the specified domain name.

    // Build the expected IngressRouteTCP matcher we expect to find
    let newest_matcher = format!("HostSNI(`{subdomain}.{basedomain}`)");

    if !present_matchers_list.contains(&newest_matcher) {
        // In this block, we are creating a new IngressRouteTCP

        // Pick a name for a new ingress route tcp that doesn't already exist
        let mut index = 0;
        let mut ingress_route_tcp_name_new = format!("{}{}", ingress_route_tcp_name_prefix_rw, index);
        while present_ing_route_tcp_names_list.contains(&ingress_route_tcp_name_new) {
            index += 1;
            ingress_route_tcp_name_new = format!("{}{}", ingress_route_tcp_name_prefix_rw, index);
        }
        let ingress_route_tcp_name_new = ingress_route_tcp_name_new;

        let ingress_route_tcp_to_apply = postgres_ingress_route_tcp(
            ingress_route_tcp_name_new.clone(),
            namespace.to_string(),
            owner_reference.clone(),
            labels.clone(),
            newest_matcher.clone(),
            service_name_read_write.to_string(),
            port.clone(),
        );
        // Apply this ingress route tcp
        let patch = Patch::Apply(&ingress_route_tcp_to_apply);
        let patch_parameters = PatchParams::apply("cntrlr").force();
        match ingress_route_tcp_api
            .patch(&ingress_route_tcp_name_new, &patch_parameters, &patch)
            .await
        {
            Ok(_) => {
                info!(
                    "Created new postgres read and write IngressRouteTCP {}.{}",
                    ingress_route_tcp_name_new, namespace
                );
            }
            Err(e) => {
                error!(
                    "Failed to create new postgres read and write IngressRouteTCP {}.{}: {}",
                    ingress_route_tcp_name_new, namespace, e
                );
                return Err(OperatorError::IngressRouteTCPCreateError);
            }
        }
    } else {
        debug!(
            "There is already an IngressRouteTCP for this matcher, so we don't need to create a new one: {}",
            newest_matcher
        );
    }

    Ok(())
}
