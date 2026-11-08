//! JVM corpus entry selection from the emitted canonical module boundary.
//!
//! The class inventory lives with the shared compile path. This module holds the regression that
//! the final source module, not a same-named dependency, owns `box()`.

#[cfg(test)]
mod tests {
    use super::super::super::box_byte_score;
    use super::super::super::box_jvm_compile::find_box_class;
    use super::super::super::{box_reference_classes, common};
    use super::super::compile_box_case;
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
