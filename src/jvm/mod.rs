//! JVM class-file reading, writing, and bytecode emission (all JVM-specific code).

mod annotation_constructions;
pub mod backend;
pub mod bridges;
pub mod classfile;
pub mod classpath;
pub mod classreader;
mod common_metadata;
pub mod companion;
mod external_calls;
mod function_classifiers;
mod function_references;
mod generic_erasure;
pub mod inline;
pub mod inline_class;
pub mod ir_emit;
pub mod java_stub;
pub mod jvm_class_map;
pub mod jvm_libraries;
mod local_properties;
mod mapped_builtin_declarations;
pub mod metadata;
mod module_calls;
pub mod names;
pub mod property_annotations;
mod property_references;
pub mod property_storage;
mod ranges;
mod shared_captures;
pub mod suspend;
mod top_level_properties;
mod value_class_declarations;
pub mod value_classes;

pub use backend::{prepare_module_symbols, JvmBackend};

/// Kotlin distribution artifacts used by JVM command-line defaults.
pub fn kotlin_stdlib_jar() -> Option<std::path::PathBuf> {
    crate::toolchain::stdlib_jar()
}

pub fn kotlin_dist_jar(name: &str) -> Option<std::path::PathBuf> {
    crate::toolchain::dist_jar(name)
}
