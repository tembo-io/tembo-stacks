use actix_web::{middleware, web, App, HttpServer};

use actix_cors::Cors;
use dataplane_webserver::{
    config,
    routes::health::{lively, ready},
    routes::root,
};
use log::info;

use dataplane_webserver::routes::metrics;
use utoipa::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa_swagger_ui::{SwaggerUi, Url};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("debug"));

    let cfg = config::Config::default();
    info!("{:?}", cfg);

    // Initializing the HTTP client during server startup
    // allows for connection pooling and re-use of TCP
    // connections to the Prometheus server.
    let http_client = reqwest::Client::builder()
        .build()
        .expect("Failed to create HTTP client");

    let security_scheme_documentation = SecurityScheme::Http(
        HttpBuilder::new().scheme(HttpAuthScheme::Bearer).bearer_format("JWT").build()
    );

    #[derive(OpenApi)]
    #[openapi(paths(metrics::query_range), components(schemas()), security(("http" = ["metrics::query_range"])))]
    struct ApiDoc;

    HttpServer::new(move || {
        let mut doc = ApiDoc::openapi();
        doc.info.description = Some("Dataplane API".to_string());
        doc.info.license = None;

        let cors = Cors::permissive();
        App::new()
            .app_data(web::Data::new(cfg.clone()))
            .app_data(web::Data::new(http_client.clone()))
            .wrap(cors)
            .wrap(middleware::Logger::default())
            .service(web::scope("/").service(root::ok))
            .service(web::scope("/{namespace}/metrics").service(metrics::query_range))
            .service(web::scope("/health").service(ready).service(lively))
            .service(SwaggerUi::new("/swagger-ui/{_:.*}").urls(vec![(
                Url::new("dataplane-api", "/api-docs/openapi.json"),
                doc,
            )]))
    })
    .workers(8)
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}
