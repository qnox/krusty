//! The JVM class that holds each source file's top-level declarations: kotlinc's package part,
//! `FooKt` after the file name, or the name a checked `@file:JvmName("Facade")` gives it, in the
//! package a checked `@file:JvmPackageName("a.b")` names instead of the file's own.
//!
//! A `@file:JvmMultifileClass` source with a `@file:JvmName` declares its members in a part of its
//! own, `Facade__FooKt`, and every part of one facade is invoked through the public facade class the
//! module writes once all of them are emitted.

use std::sync::Arc;

use crate::ir_emit::KotlinMetadata;
use crate::multifile_facades;
use krusty::backend::BackendModuleFacts;
use krusty::fir::SourceFileId;
use krusty::ir::IrFile;
use krusty::jvm::classfile::ClassWriter;
use krusty::types::{type_name, type_name_child, AnnotationValue, TypeName};

/// The JVM class that declares a source's top-level members, and where it sits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FacadeClass {
    /// The simple name of the class that declares a source's members: its package part, or its
    /// multifile part.
    pub(crate) simple: String,
    /// The simple name of the public facade a multifile source's part is invoked through.
    pub(crate) multifile_facade: Option<String>,
    /// The JVM package a checked `@file:JvmPackageName` places the class in, which is the whole
    /// package and not a suffix of the Kotlin one; `None` keeps the file's own package.
    jvm_package: Option<String>,
}

impl FacadeClass {
    /// A facade class that stays in the package of its file.
    #[cfg(test)]
    pub(crate) fn in_source_package(simple: String) -> Self {
        Self {
            simple,
            multifile_facade: None,
            jvm_package: None,
        }
    }

    /// The class's internal name for a file whose Kotlin package is `kotlin_package` (`p/q/FooKt`).
    pub(crate) fn internal_name(&self, kotlin_package: Option<&str>) -> String {
        self.internal_in(kotlin_package, &self.simple)
    }

    /// The internal name of the public facade a multifile part is invoked through.
    pub(crate) fn multifile_internal_name(&self, kotlin_package: Option<&str>) -> Option<String> {
        let facade = self.multifile_facade.as_deref()?;
        Some(self.internal_in(kotlin_package, facade))
    }

    fn internal_in(&self, kotlin_package: Option<&str>, simple: &str) -> String {
        match self.jvm_package.as_deref().or(kotlin_package) {
            Some(package) if !package.is_empty() => {
                format!("{}/{simple}", package.replace('.', "/"))
            }
            _ => simple.to_string(),
        }
    }

    /// The class's identity for a source of Kotlin package `kotlin_package`.
    pub(crate) fn type_name(&self, kotlin_package: TypeName) -> TypeName {
        self.type_name_of(kotlin_package, &self.simple)
    }

    /// The identity of the public facade a multifile part is invoked through.
    pub(crate) fn multifile_type_name(&self, kotlin_package: TypeName) -> Option<TypeName> {
        let facade = self.multifile_facade.as_deref()?;
        Some(self.type_name_of(kotlin_package, facade))
    }

    fn type_name_of(&self, kotlin_package: TypeName, simple: &str) -> TypeName {
        match self.jvm_package.as_deref() {
            Some(package) => type_name(&format!("{}/{simple}", package.replace('.', "/"))),
            None => type_name_child(kotlin_package, simple),
        }
    }
}

/// The Kotlin package a file class records as its `@Metadata.pn` when `@file:JvmPackageName` put
/// the class in another JVM package than `kotlin_package`.
pub(crate) fn relocated_package_name(class: &str, kotlin_package: &str) -> Option<String> {
    let directory = class
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    (directory != kotlin_package.replace('.', "/")).then(|| kotlin_package.to_string())
}

/// Writes the header of `class`, the class that declares `ir`'s top-level members: the `pn` of a
/// class `@file:JvmPackageName` relocated, then either the part header of a `multifile_facade`
/// part or a plain package part's file annotations and `@Metadata`.
pub(crate) fn write_header(
    cw: &mut ClassWriter,
    ir: &IrFile,
    metadata: Option<&KotlinMetadata>,
    class: &str,
    multifile_facade: Option<&str>,
) {
    if let Some(package) = relocated_package_name(class, ir.package.as_deref().unwrap_or_default())
    {
        cw.set_metadata_package_name(&package);
    }
    if let Some(multifile_facade) = multifile_facade {
        multifile_facades::write_part_header(cw, ir, metadata, multifile_facade);
    } else {
        cw.set_class_annotations(&ir.file_annotations);
        if let Some(m) = metadata {
            cw.set_kotlin_metadata(m.k, m.stamp, m.xi, &m.d1, &m.d2);
        }
    }
}

/// Each source's top-level class, indexed by source like `stems`.
#[derive(Clone, Debug)]
pub(crate) struct FileClasses {
    pub(crate) containers: Arc<[FacadeClass]>,
}

impl FileClasses {
    pub(crate) fn of(stems: &[String], module: &BackendModuleFacts) -> Self {
        let containers = stems
            .iter()
            .enumerate()
            .map(|(source, stem)| {
                let source = SourceFileId::from_raw(source as u32);
                let segment = krusty::names::file_part_short_name(stem);
                let jvm_package = declared_jvm_package(module, source);
                // kotlinc splits a multifile class only under a declared facade name; without
                // `@JvmName` the file is an ordinary package part.
                match declared_jvm_name(module, source) {
                    Some(facade) if is_multifile_part(module, source) => FacadeClass {
                        simple: format!("{facade}__{segment}"),
                        multifile_facade: Some(facade),
                        jvm_package,
                    },
                    declared => FacadeClass {
                        simple: declared.unwrap_or(segment),
                        multifile_facade: None,
                        jvm_package,
                    },
                }
            })
            .collect::<Vec<_>>();
        Self {
            containers: containers.into(),
        }
    }
}

/// The value of `source`'s `@file:JvmPackageName`, matched by its resolved identity.
fn declared_jvm_package(module: &BackendModuleFacts, source: SourceFileId) -> Option<String> {
    declared_string(module, source, type_name("kotlin/jvm/JvmPackageName"))
}

/// The value of `source`'s `@file:JvmName`, matched by its resolved identity.
fn declared_jvm_name(module: &BackendModuleFacts, source: SourceFileId) -> Option<String> {
    declared_string(module, source, type_name("kotlin/jvm/JvmName"))
}

/// The string argument of `source`'s file annotation `annotation`.
fn declared_string(
    module: &BackendModuleFacts,
    source: SourceFileId,
    annotation: TypeName,
) -> Option<String> {
    module
        .file_annotations(source)
        .iter()
        .filter(|application| application.annotation == annotation)
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
