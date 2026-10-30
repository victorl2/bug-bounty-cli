//! `bb`: discover bug bounties and audit competitions across platforms, read their rules and
//! scope, and inspect in-scope EVM contracts.

pub mod cache;
pub mod cli;
pub mod error;
pub mod evm;
pub mod http;
pub mod jv;
pub mod opts;
pub mod paths;
pub mod providers;
pub mod render;
pub mod rsc;
pub mod sources;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
