use super::*;

/// Exercise core declarations without an artifact supplying or repairing their generic layout.
/// Class-literal support is a separate platform capability from the declaration inventory.
struct CoreClassLiteralPlatform;

impl crate::symbol_source::SymbolSource for CoreClassLiteralPlatform {
    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<ResolvedSymbols> {
        crate::symbol_source::SymbolSource::symbols(&EmptySymbolSource, namespace, name)
    }
}

impl crate::libraries::SemanticPlatform for CoreClassLiteralPlatform {
    fn class_literal_type(&self) -> Option<Ty> {
        Some(Ty::obj("kotlin/reflect/KClass"))
    }
}

#[test]
fn core_array_declarations_keep_written_arguments_and_class_literal_projections() {
    for bare_array_literal in [false, true] {
        let directive = if bare_array_literal { "+" } else { "-" };
        let bare = if bare_array_literal {
            "val bare = Array::class\n"
        } else {
            ""
        };
        let source = format!(
            "// LANGUAGE: {directive}BareArrayClassLiteral\n\
             val selected: Array<String>? = null\n\
             val projected: Array<*>? = null\n\
             val explicit = Array<String>::class\n\
             {bare}"
        );
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &[SourceInput::kotlin(&source)],
            PlatformProvider::jvm(Box::new(CoreClassLiteralPlatform)),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert!(analysis.streamed.is_some());
        let actual = analysis
            .symbols
            .source_props
            .values()
            .map(|property| (property.name.as_str(), property.ty))
            .collect::<crate::fx_hash::HashMap<_, _>>();
        let mut expected = crate::fx_hash::HashMap::from_iter([
            (
                "selected",
                Ty::nullable(Ty::obj_args("kotlin/Array", &[Ty::String])),
            ),
            (
                "projected",
                Ty::nullable(Ty::obj_args(
                    "kotlin/Array",
                    &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
                )),
            ),
            (
                "explicit",
                Ty::obj_args(
                    "kotlin/reflect/KClass",
                    &[Ty::obj_args("kotlin/Array", &[Ty::String])],
                ),
            ),
        ]);
        if bare_array_literal {
            expected.insert(
                "bare",
                Ty::obj_args(
                    "kotlin/reflect/KClass",
                    &[Ty::obj_args(
                        "kotlin/Array",
                        &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
                    )],
                ),
            );
        }
        assert_eq!(
            actual, expected,
            "BareArrayClassLiteral={bare_array_literal}"
        );
    }
}
