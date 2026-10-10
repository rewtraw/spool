//! Spool server: one service that finds, downloads and organizes movies and television.

pub mod acquire;
pub mod api;
pub mod app;
pub mod archive;
pub mod art;
pub mod catalog;
pub mod db;
pub mod import;
pub mod mcp;
pub mod metadata;
pub mod migrate;
pub mod models;
pub mod newznab;
pub mod plex;
pub mod scheduler;
pub mod settings;
pub mod shadow;
pub mod store;
pub mod subs;
pub mod tracker;
