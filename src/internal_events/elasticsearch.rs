use super::InternalEvent;
use metrics::counter;
use string_cache::DefaultAtom as Atom;

#[derive(Debug)]
pub struct ElasticSearchEventReceived {
    pub byte_size: usize,
    pub index: String,
}

impl InternalEvent for ElasticSearchEventReceived {
    fn emit_logs(&self) {
        trace!(message = "inserting event", index = %self.index);
    }

    fn emit_metrics(&self) {
        counter!(
            "events_processed", 1,
            "component_kind" => "sink",
            "component_type" => "elasticsearch",
        );
        counter!(
            "bytes_processed", self.byte_size as u64,
            "component_kind" => "sink",
            "component_type" => "elasticsearch",
        );
    }
}

#[derive(Debug)]
pub struct ElasticSearchMissingKeys {
    pub keys: Vec<Atom>,
}

impl InternalEvent for ElasticSearchMissingKeys {
    fn emit_logs(&self) {
        warn!(
            message = "keys do not exist on the event; dropping event.",
            missing_keys = ?self.keys,
            rate_limit_secs = 30,
        )
    }

    fn emit_metrics(&self) {
        counter!(
            "missing_keys", 1,
            "component_kind" => "sink",
            "component_type" => "elasticsearch",
        );
    }
}
