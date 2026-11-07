//! The JVM class that holds each source file's top-level declarations: kotlinc's package part,
//! `FooKt` after the file name, or the name a checked `@file:JvmName("Facade")` gives it.
//!
//! A `@file:JvmMultifileClass` source with a `@file:JvmName` declares its members in a part of its
//! own, `Facade__FooKt`, and every part of one facade is invoked through the public facade class the
//! module writes once all of them are emitted.

use std::sync::Arc;

use crate::backend::BackendModuleFacts;
use crate::fir::SourceFileId;
use crate::types::{type_name, AnnotationValue};

/// Each source's top-level class, indexed by source like `stems`.
#[derive(Clone, Debug)]
pub(crate) struct FileClasses {
    /// The simple name of the class that declares a source's members: its package part, or its
    /// multifile part.
    pub(crate) containers: Arc<[String]>,
    /// The simple name of the public facade a multifile source's part is invoked through.
    pub(crate) multifile_facades: Arc<[Option<String>]>,
}

impl FileClasses {
    pub(crate) fn of(stems: &[String], module: &BackendModuleFacts) -> Self {
        let (containers, multifile_facades): (Vec<String>, Vec<Option<String>>) = stems
            .iter()
            .enumerate()
            .map(|(source, stem)| {
                let source = SourceFileId::from_raw(source as u32);
                let segment = crate::names::file_part_short_name(stem);
                let declared = declared_jvm_name(module, source);
                // kotlinc splits a multifile class only under a declared facade name; without
                // `@JvmName` the file is an ordinary package part.
                match declared {
                    Some(facade) if is_multifile_part(module, source) => {
                        (format!("{facade}__{segment}"), Some(facade))
                    }
                    declared => (declared.unwrap_or(segment), None),
                }
            })
            .unzip();
        Self {
            containers: containers.into(),
            multifile_facades: multifile_facades.into(),
        }
    }
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

/// Whether `source` declares `@file:JvmMultifileClass`.
fn is_multifile_part(module: &BackendModuleFacts, source: SourceFileId) -> bool {
    let multifile = type_name("kotlin/jvm/JvmMultifileClass");
    module
        .file_annotations(source)
        .iter()
        .any(|application| application.annotation == multifile)
}
