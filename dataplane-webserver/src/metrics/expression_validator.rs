use log::{debug, error, info, warn};
use promql_parser::parser;
use promql_parser::parser::{Expr, VectorSelector};
use promql_parser::util::{ExprVisitor, walk_expr};
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, SystemTime};
use promql_parser::label::MatchOp;

// https://prometheus.io/docs/prometheus/latest/querying/api/
pub struct NamespaceVisitor {
    pub namespace: String,
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
    authorized_query
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
