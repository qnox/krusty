//! Read-only collection faces: kotlinc's throwing stubs for a mutable face's members, the bridges
//! of the members a class implements, the builtin members an abstract class materializes, and the
//! special bridges of a renamed member with a narrower result, and those of members inherited from a
//! Kotlin collection base.

use super::common;

#[path = "../abstract_builtin_overrides_e2e.rs"]
mod abstract_builtin_overrides_e2e;
#[path = "../collection_override_signature_e2e.rs"]
mod collection_override_signature_e2e;
#[path = "../collection_to_array_e2e.rs"]
mod collection_to_array_e2e;
#[path = "../collection_type_safe_members_e2e.rs"]
mod collection_type_safe_members_e2e;
#[path = "../kotlin_collection_superclass_bridges_e2e.rs"]
mod kotlin_collection_superclass_bridges_e2e;
#[path = "../read_only_collection_face_stubs_e2e.rs"]
mod read_only_collection_face_stubs_e2e;
#[path = "../read_only_iterator_stubs_e2e.rs"]
mod read_only_iterator_stubs_e2e;
#[path = "../read_only_list_face_stubs_e2e.rs"]
mod read_only_list_face_stubs_e2e;
#[path = "../read_only_map_face_stubs_e2e.rs"]
mod read_only_map_face_stubs_e2e;
#[path = "../read_only_set_face_stubs_e2e.rs"]
mod read_only_set_face_stubs_e2e;
#[path = "../read_only_stub_generic_override_e2e.rs"]
mod read_only_stub_generic_override_e2e;
#[path = "../read_only_stub_inheritability_e2e.rs"]
mod read_only_stub_inheritability_e2e;
#[path = "../read_only_stub_override_bridges_e2e.rs"]
mod read_only_stub_override_bridges_e2e;
#[path = "../read_only_stub_override_return_e2e.rs"]
mod read_only_stub_override_return_e2e;
#[path = "../renamed_member_narrower_result_e2e.rs"]
mod renamed_member_narrower_result_e2e;
