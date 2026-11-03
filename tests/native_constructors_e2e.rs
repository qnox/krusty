//! Native constructor lowering: selection, secondary constructors and their defaults, vararg
//! constructors, and pair construction.

use super::common;

#[path = "native_constructor_selection_e2e.rs"]
mod native_constructor_selection_e2e;
#[path = "native_pair_construction_e2e.rs"]
mod native_pair_construction_e2e;
#[path = "native_secondary_constructor_defaults_e2e.rs"]
mod native_secondary_constructor_defaults_e2e;
#[path = "native_secondary_constructors_e2e.rs"]
mod native_secondary_constructors_e2e;
#[path = "native_vararg_constructor_e2e.rs"]
mod native_vararg_constructor_e2e;
