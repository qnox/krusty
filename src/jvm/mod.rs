//! The JVM class-file layer: reading the classpath and Kotlin metadata, the class-file model and
//! writer, and the bytecode passes and inliner that rewrite written methods. Lowering checked IR
//! to bytecode is the `krusty-jvm` crate's.

pub mod array_representation;
pub mod bridge_erasure;
pub mod bytecode;
pub mod bytecode_passes;
pub mod class_node;
pub mod classfile;
pub mod classpath;
pub mod classreader;
mod common_metadata;
pub mod compilation_inputs;
pub mod emission_readiness;
pub mod emit_modes;
mod facade_registration;
pub mod function_classifiers;
pub mod inline;
pub mod inline_class;
pub mod inliner;
mod java_nullability_qualifiers;
pub mod java_stub;
pub mod jvm_class_map;
pub mod jvm_libraries;
pub mod mapped_builtin_declarations;
pub mod metadata;
pub mod method_descriptors;
pub mod method_node;
pub mod names;
mod optional_annotations;
pub mod physical_type;
pub mod protected_dependency_calls;
pub mod reified_arguments;
pub mod source_map;
pub mod string_constant;
mod top_level_properties;
pub mod type_intrinsics;
pub mod type_of;
mod value_class_declarations;

pub use facade_registration::prepare_module_symbols;
pub use java_nullability_qualifiers::JavaNullability;
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
