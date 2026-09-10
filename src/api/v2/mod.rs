//! Ordered bundle simulation served under /api/v2.
mod bundle_call_tracer;
pub mod simulate_bundle;
mod vm_trace;

use warp::{Filter, Rejection, Reply};

pub fn routes() -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    simulate_bundle::route()
}
