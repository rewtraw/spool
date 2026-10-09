//! Spool's Usenet engine: NZB in, verified files out.

mod direct;
pub mod engine;
pub mod nntp;
pub mod nzb;
pub mod par2;
mod post;
pub mod rar;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod yenc;

pub use engine::{Engine, EngineConfig, EngineError, JobState, JobStatus};
pub use nntp::ServerConfig;
