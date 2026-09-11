#![forbid(unsafe_code)]

pub mod app;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod continuity;
pub mod doctor;
pub mod error;
pub mod git;
pub mod guardian;
pub mod handoff;
pub mod model;
pub mod paths;
pub mod provider;
pub mod security;
pub mod store;
pub mod tui;
pub mod validation;
pub mod workspace;

pub const PRODUCT_NAME: &str = "ContextWake";
pub const BINARY_NAME: &str = "ctxwake";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
