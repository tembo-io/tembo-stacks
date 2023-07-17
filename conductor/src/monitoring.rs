use opentelemetry::metrics::{Counter, Meter};

#[derive(Clone)]
pub struct CustomMetrics {
    pub instances_created: Counter<u64>,
    pub instances_updated: Counter<u64>,
    pub instances_invalid_state_transition: Counter<u64>,
}

impl CustomMetrics {
    pub fn new(meter: &Meter) -> Self {
        let instances_created = meter
            .u64_counter("instances_created")
            .with_description("Total number of create requests")
            .init();
        let instances_updated = meter
            .u64_counter("instances_updated")
            .with_description("Total number of update requests")
            .init();
        let instances_invalid_state_transition = meter
            .u64_counter("instances_invalid_state_transition")
            .with_description(
                "Total number of change requests rejected due to an invalid state transition",
            )
            .init();

        Self {
            instances_created,
            instances_updated,
            instances_invalid_state_transition,
        }
    }
}