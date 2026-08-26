#![forbid(unsafe_code)]

pub mod app;
mod client_ip;
pub mod config;
pub mod presence_ws;
pub mod rate_limit;
pub mod ws;

pub use app::{AppState, build_router};
pub use config::AppConfig;
