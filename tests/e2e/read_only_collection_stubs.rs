//! Read-only collection faces: kotlinc's throwing stubs for a mutable face's members, and the
//! bridges of the members a class implements.

use super::common;

#[path = "../read_only_iterator_stubs_e2e.rs"]
mod read_only_iterator_stubs_e2e;
#[path = "../read_only_stub_cross_file_superclass_e2e.rs"]
mod read_only_stub_cross_file_superclass_e2e;
#[path = "../read_only_stub_generic_override_e2e.rs"]
mod read_only_stub_generic_override_e2e;
#[path = "../read_only_stub_inheritability_e2e.rs"]
mod read_only_stub_inheritability_e2e;
#[path = "../read_only_stub_override_bridges_e2e.rs"]
mod read_only_stub_override_bridges_e2e;
#[path = "../read_only_stub_override_return_e2e.rs"]
mod read_only_stub_override_return_e2e;
