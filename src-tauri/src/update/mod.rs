pub mod client;
pub mod platform;
pub mod protocol;
pub mod transaction;
pub use client::Updater;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CONTROL: &str = ".arkcut-update";
