//! Declaration-side operator checks grouped behind one test-module boundary.

pub(super) use super::common;

#[path = "operator_declarations_e2e/equals.rs"]
mod equals;
#[path = "operator_declarations_e2e/inc_dec.rs"]
mod inc_dec;
