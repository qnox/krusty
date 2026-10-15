//! Declaration-side operator checks grouped behind one test-module boundary.

pub(super) use super::common;

#[path = "operator_equals_declarations_e2e.rs"]
mod equals;
#[path = "operator_inc_dec_e2e.rs"]
mod inc_dec;
