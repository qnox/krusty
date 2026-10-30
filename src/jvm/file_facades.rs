//! The JVM class that holds each source file's top-level declarations: kotlinc's package part,
//! `FooKt` after the file name, or the name a checked `@file:JvmName("Facade")` gives it.

use crate::backend::BackendModuleFacts;
use crate::fir::SourceFileId;
use crate::types::{type_name, AnnotationValue};

/// Each source's facade class simple name, indexed by source like `stems`.
pub(crate) fn facade_segments(stems: &[String], module: &BackendModuleFacts) -> Vec<String> {
    stems
        .iter()
        .enumerate()
        .map(|(source, stem)| {
            let source = SourceFileId::from_raw(source as u32);
            assert!(
                !is_multifile_part(module, source),
                "the frontend admitted an unsupported @file:JvmMultifileClass source"
            );
            declared_jvm_name(module, source)
                .unwrap_or_else(|| crate::names::file_part_short_name(stem))
        })
        .collect()
}

/// The value of `source`'s `@file:JvmName`, matched by its resolved identity.
fn declared_jvm_name(module: &BackendModuleFacts, source: SourceFileId) -> Option<String> {
    let jvm_name = type_name("kotlin/jvm/JvmName");
    module
        .file_annotations(source)
        .iter()
        .filter(|application| application.annotation == jvm_name)
        .find_map(|application| match application.arguments.first() {
            Some((_, AnnotationValue::String(name))) => Some(name.to_lossy()),
            _ => None,
        })
}

/// Whether `source` declares `@file:JvmMultifileClass`, which the frontend must reject until the
/// JVM handoff carries distinct part and public-facade owners.
fn is_multifile_part(module: &BackendModuleFacts, source: SourceFileId) -> bool {
    let multifile = type_name("kotlin/jvm/JvmMultifileClass");
    module
        .file_annotations(source)
        .iter()
        .any(|application| application.annotation == multifile)
}
