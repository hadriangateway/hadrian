//! Shared database repository test infrastructure
//!
//! This module provides a test harness for running the same test logic against
//! both SQLite and PostgreSQL implementations. Tests are organized as:
//!
//! - **Unit tests (SQLite)**: Fast, in-memory tests that run with every `cargo test`
//! - **Integration tests (PostgreSQL)**: Slower tests using testcontainers, run with `cargo test -- --ignored`
//!
//! # Architecture
//!
//! Each repository has a test module (e.g., `organizations.rs`) containing:
//! - Shared test functions that take `&dyn XxxRepo`
//! - SQLite-specific setup using in-memory databases
//! - PostgreSQL-specific setup using testcontainers (marked `#[ignore]`)
//!
//! # Running tests
//!
//! ```bash
//! cargo test                       # Run fast SQLite tests only
//! cargo test -- --ignored          # Run PostgreSQL integration tests (requires Docker)
//! cargo test -- --include-ignored  # Run all tests
//! ```

mod api_keys;
mod audit_logs;
mod containers;
mod conversations;
pub mod harness;
mod model_pricing;
mod org_rbac_policies;
mod organizations;
mod projects;
mod providers;
mod responses;
#[cfg(feature = "sso")]
mod sso_group_mappings;
mod teams;
mod usage;
mod users;
