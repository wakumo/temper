use std::{env, sync::Arc};

use dashmap::DashMap;

use enso_temper::{api, config::config, errors::handle_rejection, SharedSimulationState};
use warp::Filter;

#[tokio::main]
async fn main() {
    if env::var_os("RUST_LOG").is_none() {
        // Set `RUST_LOG=ts::api=debug` to see debug logs, this only shows access logs.
        env::set_var("RUST_LOG", "ts::api=info");
    }
    pretty_env_logger::init();

    let config = config();

    let port = config.port;
    if config.api_key.is_some() {
        log::info!(
            target: "ts::api",
            "Running with API key protection"
        );
    }

    let shared_state = Arc::new(SharedSimulationState {
        evms: Arc::new(DashMap::new()),
    });

    let routes = api::routes(config, shared_state)
        .recover(handle_rejection)
        .with(warp::log("ts::api"));

    log::info!(
        target: "ts::api",
        "Starting server on port {port}"
    );
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}
