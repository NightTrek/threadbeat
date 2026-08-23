#![forbid(unsafe_code)]

pub mod app_server;
pub mod config;
pub mod control;
pub mod daemon;
pub mod mcp;
pub mod model;
pub mod schedule;
pub mod service;
pub mod store;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
