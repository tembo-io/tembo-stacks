use crate::config;
use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use log::{debug, error, info, warn};
use promql_parser::label::MatchOp;
use promql_parser::parser;
use promql_parser::parser::{Expr, VectorSelector};
use promql_parser::util::{walk_expr, ExprVisitor};
use serde::{Deserialize, Serialize};

// https://prometheus.io/docs/prometheus/latest/querying/api/

#[derive(Deserialize)]
struct RangeQuery {
    start: String,
    query: String,
    end: Option<String>,
    step: Option<String>,
}

struct NamespaceVisitor {
    namespace: String,
}

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

    // Get prometheus URL from config
    let prometheus_url = cfg.prometheus_url.clone();

    // Get the query parameters
    let query = range_query.query.clone();
    info!("request namespace: {}, query: '{}'", namespace, query);

    // Parse the query
    let abstract_syntax_tree = match parser::parse(&query) {
        Ok(ast) => ast,
        Err(e) => {
            error!("query parse error: {}", e);
            return Ok(HttpResponse::UnprocessableEntity().json("Failed to parse PromQL query"));
        }
    };

    // Create the visitor.
    let mut visitor = NamespaceVisitor {
        namespace: namespace.clone(),
    };

    // Walk the AST with the visitor.
    let all_metrics_specify_namespace = walk_expr(&mut visitor, &abstract_syntax_tree);

    // Check if we are performing an unauthorized query.
    match all_metrics_specify_namespace {
        Ok(true) => {
            // All Matchers have the namespace specified, continue with the process...
            return Ok(HttpResponse::Ok().body(""));
        }
        _ => {
            warn!("Unauthorized query");
            return Ok(
                HttpResponse::Forbidden().json("Must include namespace in all vector selectors")
            );
        }
    }
}
