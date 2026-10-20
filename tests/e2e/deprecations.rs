//! Deprecated-declaration diagnostics and migration behavior.

pub(super) use super::common;

#[path = "../deprecation_classpath_e2e.rs"]
mod deprecation_classpath_e2e;
#[path = "../deprecation_migration_e2e.rs"]
mod deprecation_migration_e2e;
#[path = "../deprecation_usage_e2e.rs"]
mod deprecation_usage_e2e;
