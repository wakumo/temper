//! Versioned HTTP routes with shared API-key protection.
pub mod v1;
pub mod v2;

use crate::{config::Config, SharedSimulationState};
use std::sync::Arc;
use warp::{Filter, Rejection, Reply};

pub fn routes(
    config: Config,
    state: Arc<SharedSimulationState>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    let api_base = warp::path("api");
    let api_base = if let Some(api_key) = config.api_key.clone() {
        let api_key_filter = warp::header::exact("X-API-KEY", Box::leak(api_key.into_boxed_str()));
        api_base.and(api_key_filter).boxed()
    } else {
        api_base.boxed()
    };

    api_base.and(
        warp::path("v1")
            .and(v1::routes(config, state))
            .or(warp::path("v2").and(v2::routes())),
    )
}
