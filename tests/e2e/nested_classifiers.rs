//! Nested-classifier scope and lookup cases grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../classifier_precedence_e2e.rs"]
mod classifier_precedence_e2e;
#[path = "../companion_nested_classifier_scope_e2e.rs"]
mod companion_nested_classifier_scope_e2e;
#[path = "../interface_nested_classifier_e2e.rs"]
mod interface_nested_classifier_e2e;
#[path = "../nested_class_annotation_scope_e2e.rs"]
mod nested_class_annotation_scope_e2e;
#[path = "../nested_class_ctor_scope_e2e.rs"]
mod nested_class_ctor_scope_e2e;
#[path = "../nested_class_supertype_e2e.rs"]
mod nested_class_supertype_e2e;
#[path = "../nested_class_unqualified_e2e.rs"]
mod nested_class_unqualified_e2e;
#[path = "../nested_classifier_forward_ref_e2e.rs"]
mod nested_classifier_forward_ref_e2e;
#[path = "../nested_header_inherited_classifier_e2e.rs"]
mod nested_header_inherited_classifier_e2e;
