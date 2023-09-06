use crate::config;
use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use log::{debug, error, info, warn};
use promql_parser::label::MatchOp;
use promql_parser::parser;
use promql_parser::parser::{Expr, VectorSelector};
use promql_parser::util::{walk_expr, ExprVisitor};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, SystemTime};

// https://prometheus.io/docs/prometheus/latest/querying/api/

#[derive(Deserialize)]
pub struct RangeQuery {
    query: String,
    start: f64,
    end: Option<f64>,
    step: Option<String>,
}

struct NamespaceVisitor {
    namespace: String,
}

// For example, a 12 hour window with a 30s step size would be 1440 data points.
const MAX_DATAPOINTS: u64 = 2000;

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
    authorized_query
}

// examples:
// 1m
// 30s
// not supported: 1h30m
fn parse_step_into_seconds(duration: &str) -> Result<u64, &'static str> {
    let mut duration = duration.to_string();
    let duration_len = duration.len();
    let duration_suffix = duration.split_off(duration_len - 1);
    let duration_value = duration.parse::<u64>();
    if duration_value.is_err() {
        return Err("Please use a step size of seconds, minutes, hours or days, for example 1m, 30s, 5m. Combined durations not supported: 1h30m")
    }
    let duration_value = duration_value.unwrap();
    let duration = match duration_suffix.as_str() {
        "s" => duration_value,
        "m" => duration_value * 60,
        "h" => duration_value * 60 * 60,
        "d" => duration_value * 60 * 60 * 24,
        _ => return Err("Please use a step size of seconds, minutes, hours or days, for example 1m, 30s, 5m. Combined durations not supported: 1h30m")
    };
    Ok(duration)
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
                    match self.pre_visit(expr_arg) {
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
        ("namespace", example="org-coredb-inst-control-plane-dev", description = "Instance namespace"),
        ("query" = inline(String), Query, example="(sum by (namespace) (max_over_time(pg_stat_activity_count{namespace=\"org-coredb-inst-control-plane-dev\"}[1h])))", description = "PromQL range query"),
        ("start" = inline(u64), Query, example="1686780828", description = "Range start, unix timestamp"),
        ("end" = inline(Option<u64>), Query, example="1686862041", description = "Range end, unix timestamp. Default is now."),
        ("step" = inline(Option<String>), Query, example="60s", description = "Step size duration string, defaults to 60s"),
    ),
    responses(
        (status = 200, description = "https://prometheus.io/docs/prometheus/latest/querying/api/#range-queries", body = Value,
        example = json!({"status":"success","data":{"resultType":"matrix","result":[{"metric":{"__name__":"up","job":"prometheus","instance":"localhost:9090"},"values":[[1435781430.781,"1"],[1435781445.781,"1"],[1435781460.781,"1"]]},{"metric":{"__name__":"up","job":"node","instance":"localhost:9091"},"values":[[1435781430.781,"0"],[1435781445.781,"0"],[1435781460.781,"1"]]}]}})
        ),
        (status = 400, description = "Parameters are missing or incorrect"),
        (status = 403, description = "Not authorized for query"),
        (status = 422, description = "Incorrectly formatted query"),
        (status = 504, description = "Request timed out on metrics backend"),
    )
)]
#[get("/query_range")]
pub async fn query_range(
    cfg: web::Data<config::Config>,
    http_client: web::Data<Client>,
    _req: HttpRequest,
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
            info!(
                "Authorized request: namespace '{}', query '{}'",
                namespace, query
            );
        }
        _ => {
            warn!(
                "Unauthorized request: namespace '{}', query '{}'",
                namespace, query
            );
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
    // Check that end - start is not greater than 1 day, plus 100 seconds
    // if end - start > 604900.0 {
    //     warn!(
    //         "Query time range too large: namespace '{}', start '{}', end '{}'",
    //         namespace, start, end
    //     );
    //     return Ok(HttpResponse::BadRequest()
    //         .json("Query time range too large, must be less than or equal to 1 day"));
    // }

    // Check that the query is not expected to return more than MAX_DATAPOINTS
    let step_duration = match parse_step_into_seconds(&step) {
        Ok(d) => d,
        Err(_) => {
            error!("Failed to parse step duration");
            return Ok(HttpResponse::InternalServerError().json("Failed to parse step duration"));
        }
    };

    // Get timeout from config
    let prometheus_timeout_ms = cfg.prometheus_timeout_ms;
    // Set reqwest timeout to 50% greater than the prometheus timeout, plus 500ms, since we
    // prefer for Prometheus to perform the timeout rather than reqwest client.
    let reqwest_timeout_ms = prometheus_timeout_ms + (prometheus_timeout_ms / 2) + 500;
    let reqwest_timeout_ms: u64 = match reqwest_timeout_ms.try_into() {
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
            return Ok(HttpResponse::InternalServerError()
                .json("Failed to create URL to query Prometheus"));
        }
    };
    debug!("{}", query_url);

    // Create an HTTP request to the Prometheus backend
    let prometheus_response = match http_client
        .get(query_url)
        .timeout(Duration::from_millis(reqwest_timeout_ms))
        .send()
        .await
    {
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
            return Ok(HttpResponse::InternalServerError()
                .json("Failed to parse Prometheus response in JSON"));
        }
    };

    match status_code {
        StatusCode::OK => {
            debug!("Request to prometheus returned 200");
        }
        StatusCode::BAD_REQUEST => {
            warn!("{:?}", &json_response);
            return Ok(
                HttpResponse::BadRequest().json("Prometheus reported the query is malformed")
            );
        }
        StatusCode::GATEWAY_TIMEOUT | StatusCode::SERVICE_UNAVAILABLE => {
            warn!("{:?}", &json_response);
            return Ok(HttpResponse::GatewayTimeout().json("Prometheus timeout"));
        }
        StatusCode::UNPROCESSABLE_ENTITY => {
            // If this is a timeout, then make the response 503 to match the other
            // types of timeouts.
            warn!("{:?}", &json_response);
            if json_response["error"]
                .to_string()
                .contains("context deadline exceeded")
            {
                return Ok(HttpResponse::GatewayTimeout().json("Prometheus timeout"));
            }
            return Ok(
                HttpResponse::BadRequest().json("Expression cannot be executed on Prometheus")
            );
        }
        _ => {
            error!("{:?}: {:?}", status_code, &json_response);
            return Ok(HttpResponse::InternalServerError()
                .json("Prometheus returned an unexpected status code"));
        }
    }

    // return json response from prometheus to client
    Ok(HttpResponse::Ok().json(json_response))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_seconds() {
        let result = parse_step_into_seconds("5s").unwrap();
        assert_eq!(result, 5);
    }

    #[test]
    fn test_parse_minutes() {
        let result = parse_step_into_seconds("2m").unwrap();
        assert_eq!(result, 2 * 60);
    }

    #[test]
    fn test_parse_hours() {
        let result = parse_step_into_seconds("3h").unwrap();
        assert_eq!(result, 3 * 60 * 60);
    }

    #[test]
    fn test_parse_days() {
        let result = parse_step_into_seconds("4d").unwrap();
        assert_eq!(result, 4 * 60 * 60 * 24);
    }

    #[test]
    fn test_parse_weeks() {
        let result = parse_step_into_seconds("1w").unwrap();
        assert_eq!(result, 7 * 60 * 60 * 24);
    }

    #[test]
    fn test_parse_years() {
        let result = parse_step_into_seconds("2y").unwrap();
        assert_eq!(result, 2 * 365 * 60 * 60 * 24);
    }

    #[test]
    fn test_parse_invalid_suffix() {
        let result = parse_step_into_seconds("5x");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "Failed to parse duration suffix");
    }

    #[test]
    fn test_parse_invalid_value() {
        let result = parse_step_into_seconds("abm");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "Failed to parse duration value");
    }

    #[test]
    fn test_empty_input() {
        let result = parse_step_into_seconds("");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "Failed to parse duration value");
    }
}
