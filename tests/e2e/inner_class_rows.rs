//! Which nested classes a class lists in `InnerClasses`, grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../classpath_inner_classes_e2e.rs"]
mod classpath_inner_classes_e2e;
#[path = "../cross_file_serializer_inner_classes_e2e.rs"]
mod cross_file_serializer_inner_classes_e2e;
#[path = "../descriptor_inner_classes_e2e.rs"]
mod descriptor_inner_classes_e2e;
#[path = "../inlined_code_inner_classes_e2e.rs"]
mod inlined_code_inner_classes_e2e;
#[path = "../type_operand_inner_classes_e2e.rs"]
mod type_operand_inner_classes_e2e;
