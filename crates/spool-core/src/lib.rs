//! Spool core: release parsing, quality model, selection decisions and naming.
//! Pure logic with no I/O, so every rule can be tested against fixtures.

pub mod decision;
pub mod matching;
pub mod mediainfo;
pub mod naming;
pub mod parser;
pub mod profile;
pub mod quality;
mod sonarr_regexes;

pub use quality::{Flavor, Quality, QualityModel, Revision};
