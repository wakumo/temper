use dashmap::DashMap;
use evm::Evm;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub mod api;
pub mod config;
pub mod errors;
pub mod evm;
pub mod quicknode;
pub mod simulation;

pub struct SharedSimulationState {
    pub evms: Arc<DashMap<Uuid, Arc<Mutex<Evm>>>>,
}
