use crate::config;
use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use log::{error, info, warn};
use promql_parser::parser;
use serde::{Deserialize, Serialize};

// https://prometheus.io/docs/prometheus/latest/querying/api/

#[derive(Deserialize)]
struct RangeQuery {
    start: String,
    query: String,
    end: Option<String>,
    step: Option<String>,
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

    // Get prometheus URL from config
    let prometheus_url = cfg.prometheus_url.clone();

    // Get the query parameters
    let query = range_query.query.clone();
    info!("request namespace: {}, query: {}", namespace, query);

    return Ok(HttpResponse::Ok().body(""));
}
