use actix_web::{get, Error, HttpRequest, HttpResponse};
use log::{error, info, warn};
use crate::config;

#[utoipa::path(
context_path = "/",
responses(
(status = 200, description = "Returns metrics", body = Value),
(status = 403, description = "Not authorized for query", body = Value),
(status = 422, description = "Incorrectly formatted request", body = Value),
)
)]
#[get("/{namespace}/metrics")]
pub async fn metrics(req: HttpRequest) -> Result<HttpResponse, Error> {

    // Get prometheus URL from config
    let prometheus_url = match req
        .app_data::<config::Config>()
        .map(|cfg| cfg.prometheus_url.clone()) {
        Some(url) => url,
        None => {
            error!("No prometheus URL configured, please set environment variable PROMETHEUS_URL");
            return Ok(HttpResponse::InternalServerError().finish());
        }
    };

    return Ok(HttpResponse::Ok().body(""));
}