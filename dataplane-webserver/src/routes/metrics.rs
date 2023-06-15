use std::time::{Duration, SystemTime};
use crate::config;
use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use log::{debug, error, info, warn};
use promql_parser::label::MatchOp;
use promql_parser::parser;
use promql_parser::parser::{Expr, VectorSelector};
use promql_parser::util::{walk_expr, ExprVisitor};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// https://prometheus.io/docs/prometheus/latest/querying/api/

#[derive(Deserialize)]
struct RangeQuery {
    query: String,
    start: f64,
    end: Option<f64>,
    step: Option<String>,
}

struct NamespaceVisitor {
    namespace: String,
}

// Vector selector is the part in prometheus query that selects the metrics
// Example: (sum by (namespace) (container_memory_usage_bytes))
// container_memory_usage_bytes is the vector selector.
// We require all vector selectors to have a label namespace
// For example like this (sum by (namespace) (container_memory_usage_bytes{namespace="org-foo-inst-bar"}))
fn validate_vector_selector(namespace: &String, vector_selector: &VectorSelector) -> bool {
    let mut authorized_query = false;
    for filters in &vector_selector.matchers.matchers {
        if filters.name == "namespace"
            && filters.value == *namespace
            && filters.op == MatchOp::Equal
        {
            authorized_query = true;
        }
    }
    return authorized_query;
}

// This checks that prometheus queries are only using authorized namespace
impl ExprVisitor for NamespaceVisitor {
    type Error = &'static str; // Using a simple error type for this example.

    fn pre_visit(&mut self, expr: &Expr) -> Result<bool, Self::Error> {
        match expr {
            Expr::VectorSelector(vector_selector) => {
                let authorized_query = validate_vector_selector(&self.namespace, vector_selector);
                if !authorized_query {
                    return Ok(false);
                }
            }
            Expr::MatrixSelector(matrix_selector) => {
                let authorized_query =
                    validate_vector_selector(&self.namespace, &matrix_selector.vector_selector);
                if !authorized_query {
                    return Ok(false);
                }
            }
            Expr::Call(call) => {
                for boxed_arg in &call.args.args {
                    let expr_arg = boxed_arg;
                    match self.pre_visit(&expr_arg) {
                        Ok(true) => (),
                        Ok(false) => return Ok(false),
                        Err(e) => return Err(e),
                    }
                }
            }
            Expr::Extension(_) => {
                return Err("Using PromQL extensions is not allowed");
            }
            _ => (),
        }
        // Continue to the rest of the tree.
        Ok(true)
    }
}

#[utoipa::path(
    context_path = "/{namespace}/metrics",
    params(
        ("namespace", example="org-tembo-inst-sample", description = "Instance namespace"),
        ("query", example="standard", description = "PromQL range query"),
        ("start", example="1686780828", description = "Range start, unix timestamp"),
        ("end", example="1686780828", description = "Range end, unix timestamp. Default is now."),
        ("step", example="60s", description = "Step size, defaults to 60s"),
    ),
    responses(
        (status = 200, description = "Metrics queries over a range", body = Value),
        (status = 400, description = "Parameters are missing or incorrect", body = Value),
        (status = 403, description = "Not authorized for query", body = Value),
        (status = 422, description = "Incorrectly formatted query", body = Value),
        (status = 504, description = "Request timed out on metrics backend", body = Value),
    )
)]
#[get("/query_range")]
pub async fn query_range(
    cfg: web::Data<config::Config>,
    req: HttpRequest,
    range_query: web::Query<RangeQuery>,
    path: web::Path<(String,)>,
) -> Result<HttpResponse, Error> {
    let (namespace,) = path.into_inner();


    // Get the query parameters
    let query = range_query.query.clone();

    // Parse the query
    let abstract_syntax_tree = match parser::parse(&query) {
        Ok(ast) => ast,
        Err(e) => {
            error!("Query parse error: {}", e);
            return Ok(HttpResponse::UnprocessableEntity().json("Failed to parse PromQL query"));
        }
    };

    // Recurse through all terms in the expression to find any terms that specify
    // label matching, and make sure all of them specify the namespace label.
    let mut visitor = NamespaceVisitor {
        namespace: namespace.clone(),
    };
    let all_metrics_specify_namespace = walk_expr(&mut visitor, &abstract_syntax_tree);

    // Check if we are performing an unauthorized query.
    match all_metrics_specify_namespace {
        Ok(true) => {
            info!("Authorized request: namespace '{}', query '{}'", namespace, query);
        }
        _ => {
            warn!("Unauthorized request: namespace '{}', query '{}'", namespace, query);
            return Ok(
                HttpResponse::Forbidden().json("Must include namespace in all vector selectors")
            );
        }
    }

    let start = range_query.start;
    // If 'end' query parameter was provided, use it. Otherwise use current time.
    let end = match range_query.end {
        Some(end) => end,
        None => match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(n) => n.as_secs_f64(),
            Err(_) => {
                error!("Failed to get current time");
                return Ok(HttpResponse::InternalServerError().json("Failed to get current time"));
            }
        },
    };
    let step = match range_query.step.clone() {
        Some(step) => step,
        None => "60s".to_string(),
    };

    // Get timeout from config
    let prometheus_timeout_ms = cfg.prometheus_timeout_ms;
    // Set reqwest timeout to 50% greater than the prometheus timeout
    let reqwest_timeout_ms = prometheus_timeout_ms + (prometheus_timeout_ms / 2);
    let reqwest_timeout_ms : u64 = match reqwest_timeout_ms.try_into() {
        Ok(n) => n,
        Err(_) => {
            error!("Failed to convert timeout to u64");
            return Ok(HttpResponse::InternalServerError().json("Failed to convert timeout"));
        }
    };
    let timeout = format!("{prometheus_timeout_ms}ms");

    let query_params = vec![
        ("query", query),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("step", step),
        ("timeout", timeout),
    ];
    // Get prometheus URL from config
    let prometheus_url = cfg.prometheus_url.clone();
    // trim trailing slash
    let prometheus_url = prometheus_url.trim_end_matches('/');
    let prometheus_url = format!("{}/api/v1/query_range", prometheus_url);

    let query_url = match Url::parse_with_params(&prometheus_url, &query_params) {
        Ok(url) => url,
        Err(e) => {
            error!("Failed to parse Prometheus URL: {}", e);
            return Ok(HttpResponse::InternalServerError().json("Failed to create URL to query Prometheus"));
        }
    };
    debug!("{}", query_url);


    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(reqwest_timeout_ms))
        .build() {
            Ok(client) => client,
            Err(e) => {
                error!("Failed to create reqwest client: {}", e);
                return Ok(HttpResponse::InternalServerError().json("Failed to create reqwest client"));
            }
        };

    // Create an HTTP request to the Prometheus backend
    let prometheus_response = match client.get(query_url).send().await {
        Ok(response) => response,
        Err(e) => {
            error!("Failed to query Prometheus: {}", e);
            return Ok(HttpResponse::GatewayTimeout().json("Failed to query Prometheus"));
        }
    };
    debug!("{:?}", &prometheus_response);

    let status_code = prometheus_response.status();

    let json_response = match prometheus_response.json::<Value>().await {
        Ok(response) => response,
        Err(e) => {
            error!("Failed to parse Prometheus response: {}", e);
            return Ok(HttpResponse::InternalServerError().json("Failed to parse Prometheus response in JSON"));
        }
    };
    debug!("{:?}", &json_response);

    match status_code {
        StatusCode::OK => {
           debug!("Request to prometheus returned 200");
            },
        StatusCode::BAD_REQUEST => {
            warn!("{:?}", &json_response);
            return Ok(HttpResponse::BadRequest().json("Prometheus reported the query is malformed"));
        },
        StatusCode::GATEWAY_TIMEOUT => {
            warn!("{:?}", &json_response);
            return Ok(HttpResponse::GatewayTimeout().json("Prometheus timeout"));
        },
        StatusCode::UNPROCESSABLE_ENTITY => {
            // If this is a timeout, then make the response 503 to match the other
            // types of timeouts.
            warn!("{:?}", &json_response);
            if json_response["error"].to_string().contains("context deadline exceeded") {
                return Ok(HttpResponse::GatewayTimeout().json("Prometheus timeout"));
            }
            return Ok(HttpResponse::BadRequest().json("Expression cannot be executed on Prometheus"));
        },
        _ => {
            error!("{:?}: {:?}", status_code, &json_response);
            return Ok(HttpResponse::InternalServerError().json("Prometheus returned an unexpected status code"));
        }
    }

    // return json response from prometheus to client
    Ok(HttpResponse::Ok().json(json_response))
}
