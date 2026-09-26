//! JVM class-file reading, writing, and bytecode emission (all JVM-specific code).

mod abstract_method_nullability;
mod annotation_constructions;
mod array_representation;
pub mod backend;
mod bridge_return_adaptations;
pub mod bridges;
mod builtin_member_operations;
mod bytecode;
pub(crate) mod bytecode_passes;
mod call_result_boundaries;
pub(crate) mod class_node;
pub mod classfile;
pub mod classpath;
pub mod classreader;
mod common_metadata;
pub mod companion;
pub mod compilation_inputs;
mod constructor_debug;
mod constructor_metadata;
mod debug_local_names;
mod declaration_collisions;
mod default_call_operands;
mod external_calls;
pub mod frame_audit;
mod fresh_storage;
mod function_classifiers;
mod function_references;
mod generated_member_metadata;
mod generic_erasure;
pub mod inline;
pub mod inline_class;
pub(crate) mod inliner;
mod inner_classes;
pub mod ir_emit;
pub mod java_stub;
pub mod jvm_class_map;
pub mod jvm_libraries;
mod lifted_names;
mod local_class_names;
mod local_classifiers;
mod local_properties;
mod mapped_builtin_declarations;
pub mod metadata;
mod metadata_flags;
mod metadata_method_signatures;
pub mod method_node;
mod method_parameters;
mod module_calls;
pub mod names;
mod parameter_assertions;
mod parameter_names;
pub mod property_annotations;
mod property_realizations;
mod property_references;
pub mod property_storage;
mod ranges;
mod reified_arguments;
mod reified_operations;
mod runtime_capabilities;
mod shared_captures;
pub mod source_map;
pub mod suspend;
pub(crate) mod suspend_impls;
mod top_level_properties;
mod type_intrinsics;
mod type_of;
mod value_class_declarations;
pub mod value_classes;

pub use backend::{prepare_module_symbols, JvmBackend};
// Public JVM clients should not need to reach through the compiler's semantic-platform ownership
// module merely to name the initialization error returned by `JvmLibraries`.
pub use crate::libraries::PlatformInitializationError;

/// Kotlin distribution artifacts used by JVM command-line defaults.
pub fn kotlin_stdlib_jar() -> Option<std::path::PathBuf> {
    crate::toolchain::stdlib_jar()
}

pub fn kotlin_dist_jar(name: &str) -> Option<std::path::PathBuf> {
    crate::toolchain::dist_jar(name)
}
