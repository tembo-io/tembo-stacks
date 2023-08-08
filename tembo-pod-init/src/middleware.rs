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
        if ["/health/liveness", "/health/readiness"].contains(&request.path()) {
            // Don't create a span for the specified paths
            return Span::none();
        }

        // For all other paths, create a span with INFO level
        tracing_actix_web::root_span!(level = Level::INFO, request)
    }

    fn on_request_end<B: MessageBody>(span: Span, outcome: &Result<ServiceResponse<B>, Error>) {
        DefaultRootSpanBuilder::on_request_end(span, outcome);
    }
}
