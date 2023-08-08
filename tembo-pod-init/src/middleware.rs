use actix_web::{
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    Error,
};
use tracing::*;
use tracing_actix_web::{DefaultRootSpanBuilder, Level, RootSpanBuilder};

// Create a custom RootSpanBuilder
pub struct CustomLevelRootSpanBuilder;

impl RootSpanBuilder for CustomLevelRootSpanBuilder {
    fn on_request_start(request: &ServiceRequest) -> Span {
        // If the path matches our excluded routes, set the log level to ERROR (to effectively mute it)
        // Else, set the log level to INFO.
        let level = match request.path() {
            "/health/liveness" | "/health/readiness" => Level::ERROR,
            _ => Level::INFO,
        };
        tracing_actix_web::root_span!(level = level, request)
    }

    fn on_request_end<B: MessageBody>(span: Span, outcome: &Result<ServiceResponse<B>, Error>) {
        DefaultRootSpanBuilder::on_request_end(span, outcome);
    }
}
