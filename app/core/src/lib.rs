//! Shared Moni telemetry code: the Windows service runs [`engine::run`], and
//! the desktop app reads the same config/status types.

pub mod config;
pub mod engine;
pub mod gpu;
pub mod hardware;
pub mod paths;
pub mod sampler;
pub mod transport;
