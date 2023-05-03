use crate::coredb_crd::CoreDBExtensions;
use log::info;
use std::collections::HashSet;

pub fn diff_extensions(desired: &[CoreDBExtensions], actual: &[CoreDBExtensions]) -> bool {
    let set_desired: HashSet<_> = desired.iter().cloned().collect();

    let set_actual: HashSet<_> = actual.iter().cloned().collect();
    let diff: Vec<CoreDBExtensions> = set_desired.difference(&set_actual).cloned().collect();
    if !diff.is_empty() {
        info!("Difference in extensions: {:?}", diff);
        true
    } else {
        false
    }
}
