// WARNING: generated by kopium - manual changes will be overwritten
// kopium command: kopium ingressroutes.traefik.containo.us -A --derive Default
// kopium version: 0.14.0

use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// IngressRouteSpec defines the desired state of IngressRoute.
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "traefik.containo.us",
    version = "v1alpha1",
    kind = "IngressRoute",
    plural = "ingressroutes"
)]
#[kube(namespaced)]
pub struct IngressRouteSpec {
    /// EntryPoints defines the list of entry point names to bind to. Entry points have to be configured in the static configuration. More info: https://doc.traefik.io/traefik/v2.9/routing/entrypoints/ Default: all.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "entryPoints")]
    pub entry_points: Option<Vec<String>>,
    /// Routes defines the list of routes.
    pub routes: Vec<IngressRouteRoutes>,
    /// TLS defines the TLS configuration. More info: https://doc.traefik.io/traefik/v2.9/routing/routers/#tls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<IngressRouteTls>,
}

/// Route holds the HTTP route configuration.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteRoutes {
    /// Kind defines the kind of the route. Rule is the only supported kind.
    pub kind: IngressRouteRoutesKind,
    /// Match defines the router's rule. More info: https://doc.traefik.io/traefik/v2.9/routing/routers/#rule
    #[serde(rename = "match")]
    pub r#match: String,
    /// Middlewares defines the list of references to Middleware resources. More info: https://doc.traefik.io/traefik/v2.9/routing/providers/kubernetes-crd/#kind-middleware
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub middlewares: Option<Vec<IngressRouteRoutesMiddlewares>>,
    /// Priority defines the router's priority. More info: https://doc.traefik.io/traefik/v2.9/routing/routers/#priority
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Services defines the list of Service. It can contain any combination of TraefikService and/or reference to a Kubernetes Service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<IngressRouteRoutesServices>>,
}

/// Route holds the HTTP route configuration.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum IngressRouteRoutesKind {
    #[default]
    Rule,
}

/// MiddlewareRef is a reference to a Middleware resource.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteRoutesMiddlewares {
    /// Name defines the name of the referenced Middleware resource.
    pub name: String,
    /// Namespace defines the namespace of the referenced Middleware resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Service defines an upstream HTTP service to proxy traffic to.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteRoutesServices {
    /// Kind defines the kind of the Service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IngressRouteRoutesServicesKind>,
    /// Name defines the name of the referenced Kubernetes Service or TraefikService. The differentiation between the two is specified in the Kind field.
    pub name: String,
    /// Namespace defines the namespace of the referenced Kubernetes Service or TraefikService.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// PassHostHeader defines whether the client Host header is forwarded to the upstream Kubernetes Service. By default, passHostHeader is true.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "passHostHeader")]
    pub pass_host_header: Option<bool>,
    /// Port defines the port of a Kubernetes Service. This can be a reference to a named port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<IntOrString>,
    /// ResponseForwarding defines how Traefik forwards the response from the upstream Kubernetes Service to the client.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "responseForwarding"
    )]
    pub response_forwarding: Option<IngressRouteRoutesServicesResponseForwarding>,
    /// Scheme defines the scheme to use for the request to the upstream Kubernetes Service. It defaults to https when Kubernetes Service port is 443, http otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// ServersTransport defines the name of ServersTransport resource to use. It allows to configure the transport between Traefik and your servers. Can only be used on a Kubernetes Service.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "serversTransport")]
    pub servers_transport: Option<String>,
    /// Sticky defines the sticky sessions configuration. More info: https://doc.traefik.io/traefik/v2.9/routing/services/#sticky-sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sticky: Option<IngressRouteRoutesServicesSticky>,
    /// Strategy defines the load balancing strategy between the servers. RoundRobin is the only supported value at the moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Weight defines the weight and should only be specified when Name references a TraefikService object (and to be precise, one that embeds a Weighted Round Robin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
}

/// Service defines an upstream HTTP service to proxy traffic to.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub enum IngressRouteRoutesServicesKind {
    Service,
    TraefikService,
}

/// ResponseForwarding defines how Traefik forwards the response from the upstream Kubernetes Service to the client.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteRoutesServicesResponseForwarding {
    /// FlushInterval defines the interval, in milliseconds, in between flushes to the client while copying the response body. A negative value means to flush immediately after each write to the client. This configuration is ignored when ReverseProxy recognizes a response as a streaming response; for such responses, writes are flushed to the client immediately. Default: 100ms
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "flushInterval")]
    pub flush_interval: Option<String>,
}

/// Sticky defines the sticky sessions configuration. More info: https://doc.traefik.io/traefik/v2.9/routing/services/#sticky-sessions
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteRoutesServicesSticky {
    /// Cookie defines the sticky cookie configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<IngressRouteRoutesServicesStickyCookie>,
}

/// Cookie defines the sticky cookie configuration.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteRoutesServicesStickyCookie {
    /// HTTPOnly defines whether the cookie can be accessed by client-side APIs, such as JavaScript.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "httpOnly")]
    pub http_only: Option<bool>,
    /// Name defines the Cookie name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// SameSite defines the same site policy. More info: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Set-Cookie/SameSite
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sameSite")]
    pub same_site: Option<String>,
    /// Secure defines whether the cookie can only be transmitted over an encrypted connection (i.e. HTTPS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
}

/// TLS defines the TLS configuration. More info: https://doc.traefik.io/traefik/v2.9/routing/routers/#tls
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteTls {
    /// CertResolver defines the name of the certificate resolver to use. Cert resolvers have to be configured in the static configuration. More info: https://doc.traefik.io/traefik/v2.9/https/acme/#certificate-resolvers
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "certResolver")]
    pub cert_resolver: Option<String>,
    /// Domains defines the list of domains that will be used to issue certificates. More info: https://doc.traefik.io/traefik/v2.9/routing/routers/#domains
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<IngressRouteTlsDomains>>,
    /// Options defines the reference to a TLSOption, that specifies the parameters of the TLS connection. If not defined, the `default` TLSOption is used. More info: https://doc.traefik.io/traefik/v2.9/https/tls/#tls-options
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<IngressRouteTlsOptions>,
    /// SecretName is the name of the referenced Kubernetes Secret to specify the certificate details.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "secretName")]
    pub secret_name: Option<String>,
    /// Store defines the reference to the TLSStore, that will be used to store certificates. Please note that only `default` TLSStore can be used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<IngressRouteTlsStore>,
}

/// Domain holds a domain name with SANs.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteTlsDomains {
    /// Main defines the main domain name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// SANs defines the subject alternative domain names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sans: Option<Vec<String>>,
}

/// Options defines the reference to a TLSOption, that specifies the parameters of the TLS connection. If not defined, the `default` TLSOption is used. More info: https://doc.traefik.io/traefik/v2.9/https/tls/#tls-options
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteTlsOptions {
    /// Name defines the name of the referenced TLSOption. More info: https://doc.traefik.io/traefik/v2.9/routing/providers/kubernetes-crd/#kind-tlsoption
    pub name: String,
    /// Namespace defines the namespace of the referenced TLSOption. More info: https://doc.traefik.io/traefik/v2.9/routing/providers/kubernetes-crd/#kind-tlsoption
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Store defines the reference to the TLSStore, that will be used to store certificates. Please note that only `default` TLSStore can be used.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct IngressRouteTlsStore {
    /// Name defines the name of the referenced TLSStore. More info: https://doc.traefik.io/traefik/v2.9/routing/providers/kubernetes-crd/#kind-tlsstore
    pub name: String,
    /// Namespace defines the namespace of the referenced TLSStore. More info: https://doc.traefik.io/traefik/v2.9/routing/providers/kubernetes-crd/#kind-tlsstore
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}
