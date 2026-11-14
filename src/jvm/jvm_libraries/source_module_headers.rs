//! Java source files compiled beside Kotlin sources contribute declaration headers before Kotlin
//! signature resolution. This module installs them as a stub overlay over the classpath.

use super::JvmLibraries;
use crate::types::TypeName;

impl JvmLibraries {
    pub(super) fn install_java_source_headers(
        &self,
        sources: &[crate::libraries::PlatformSourceHeaderInput<'_>],
        source_classifiers: &[(TypeName, crate::types::Visibility)],
    ) -> Result<(), crate::libraries::SourceHeaderError> {
        let java = sources
            .iter()
            .map(|source| {
                (
                    source.source,
                    source.file_stem.map_or_else(
                        || format!("Source{}.java", source.source),
                        |stem| format!("{stem}.java"),
                    ),
                    source.text.to_string(),
                )
            })
            .collect::<Vec<_>>();
        if java.is_empty() {
            return Ok(());
        }
        if self.source_headers.borrow().is_some() {
            return Err(crate::libraries::SourceHeaderError {
                source: java[0].0,
                message: "JVM source-module headers were installed more than once".into(),
            });
        }
        let source_classifiers = source_classifiers
            .iter()
            .copied()
            .collect::<crate::fx_hash::HashMap<_, _>>();
        let world = crate::jvm::java_stub::SourceModuleHeaderWorld {
            source_classifiers: &source_classifiers,
            classpath: &self.cp,
        };
        let java_blocks = java
            .iter()
            .map(|(_, name, source)| (name.clone(), source.clone()))
            .collect::<Vec<_>>();
        // Lenient, like kotlinc: one unresolvable Java type must not withdraw every Java header of
        // the module (`an_unresolved_type_in_one_java_source_keeps_the_module_java_headers`).
        let headers = crate::jvm::java_stub::stub_classes(
            &java_blocks,
            crate::jvm::java_stub::StubMode::Lenient,
            &world,
        )
        .expect("lenient Java header stubbing keeps every source set");
        *self.source_headers.borrow_mut() = Some(self.cp.push_stub_overlay(headers));
        Ok(())
    }
}
