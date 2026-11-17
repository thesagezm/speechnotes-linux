//! Core shared utilities: Apple date encoding, atomic store files, logging.
pub mod appledate;
pub mod atomic;
pub mod logging;
pub mod paths;
pub mod util;

pub use appledate::AppleDate;
// `sn_core::LogRing::new()` is called from bin/main.rs and sn-ui window.rs.
pub use logging::LogRing;
pub mod logcap;
pub use logcap::CappedLog;
