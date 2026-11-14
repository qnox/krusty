//! JVM corpus entry selection from the emitted canonical module boundary.

use super::{box_byte_score, box_reference_classes};
use krusty::jvm::classreader::parse_class;

/// krusty's emitted classes for one box case: the flat list the JVM runner loads, plus the module
/// boundaries byte scoring pairs on. Every class is kept once in `flat`; `module_ranges` records
/// each module's contiguous half-open range into it, in emission order. A single-module topology
/// holds one range covering every class under [`box_reference_classes::MAIN_MODULE`]; a multi-module
/// build records one range per module so a class name shared by two modules stays two identities.
pub(in super::super) struct CompiledClasses {
    pub(in super::super) flat: Vec<(String, Vec<u8>)>,
    pub(in super::super) module_ranges: Vec<(String, std::ops::Range<usize>)>,
}

impl CompiledClasses {
    /// Wrap a single module's flat class list (ordinary, `// FILE:`, or mixed-Java topology).
    pub(super) fn single_module(flat: Vec<(String, Vec<u8>)>) -> Self {
        let range = 0..flat.len();
        CompiledClasses {
            module_ranges: vec![(box_reference_classes::MAIN_MODULE.to_string(), range)],
            flat,
        }
    }

    /// Only the final canonical `ModuleUnit` owns the executable box entry, even if its
    /// range is empty. Dependencies remain in `flat` and their complete scoring ranges.
    pub(super) fn box_class(&self) -> Option<String> {
        let (_, range) = self.module_ranges.last()?;
        find_box_class(self.flat.get(range.clone())?)
    }

    /// The module-qualified inventory for byte scoring, keyed by `(module, internal name)`.
    pub(super) fn qualified(&self) -> box_byte_score::QualifiedClasses {
        let mut out = box_byte_score::QualifiedClasses::new();
        for (module, range) in &self.module_ranges {
            for (name, bytes) in &self.flat[range.clone()] {
                out.insert((module.clone(), name.clone()), bytes.clone());
            }
        }
        out
    }
}

/// Find the class that declares `static box()Ljava/lang/String;`.
pub(super) fn find_box_class(classes: &[(String, Vec<u8>)]) -> Option<String> {
    for (name, bytes) in classes {
        if let Ok(ci) = parse_class(bytes) {
            if ci
                .method("box", "()Ljava/lang/String;")
                .is_some_and(|m| m.is_static())
            {
                return Some(name.replace('/', "."));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::{common, compile_box_case};
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn packaged_final_module_box_is_the_product_corpus_entry() {
        let source = krusty::conformance::prepare_test_source(
            include_str!("../box_regressions/multimodule/packagedFinalModuleBox.kt"),
            krusty::conformance::TestTarget::Jvm,
        );
        let reference =
            box_reference_classes::checked_prepared_box(&source, "packagedFinalModuleBox")
                .expect("the official compiler executes the whole final-module program as OK");
        let classpath = common::classpath_jars_for(&source);
        let jdk = common::jdk_modules();
        let compiled = compile_box_case(
            &source,
            "packagedFinalModuleBox",
            &classpath,
            Some(jdk.as_path()),
            &|_| {},
        )
        .expect("the product compiles the whole portable module graph");
        assert_eq!(
            compiled.qualified().into_keys().collect::<BTreeSet<_>>(),
            box_byte_score::qualified_from_reference(&reference)
                .into_keys()
                .collect::<BTreeSet<_>>(),
            "both complete module-qualified class inventories are retained",
        );
        assert_eq!(
            compiled
                .module_ranges
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["library", "main"],
        );
        let (_, library) = &compiled.module_ranges[0];
        let library_box = find_box_class(&compiled.flat[library.clone()])
            .expect("the whole library decoy really emits a callable box");
        assert_eq!(
            common::run_box(&compiled.flat, &library_box, &classpath),
            Some("library box".to_string()),
        );
        let box_class = compiled
            .box_class()
            .expect("the final module emits its entry");
        assert_eq!(box_class, "packageentry.when.MainKt");
        assert_eq!(
            common::run_box(&compiled.flat, &box_class, &classpath),
            Some("OK".to_string()),
        );

        // PLATFORM-SPECIFIC: exercise the emitted-module boundary, retaining the real class
        // bytes while modeling a final compilation unit that emits no classes.
        let mut empty_final = compiled;
        let end = empty_final.flat.len();
        empty_final.module_ranges.last_mut().unwrap().1 = end..end;
        assert_eq!(
            empty_final.box_class(),
            None,
            "an empty final module must not fall back to the executable library decoy",
        );
    }
}
