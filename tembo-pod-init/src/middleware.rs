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
        // If the path matches our excluded routes, do not generate a span.
        match request.path() {
            "/health/liveness" | "/health/readiness" => {
                // This is the crucial part: returning a non-recording span
                // effectively "turns off" tracing for this request.
                Span::none()
            }
            _ => tracing_actix_web::root_span!(level = Level::INFO, request),
        }
    }

    fn on_request_end<B: MessageBody>(span: Span, outcome: &Result<ServiceResponse<B>, Error>) {
        DefaultRootSpanBuilder::on_request_end(span, outcome);
    }
}
