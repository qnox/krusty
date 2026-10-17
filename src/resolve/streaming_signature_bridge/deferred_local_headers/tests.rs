use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

#[test]
fn deferred_local_classifier_publishes_header_and_generic_layout_atomically() {
    let source = r#"
package sample

class Target<T>(val declarationSite: Int)
typealias Alias<T> = Target<T>
class Leaf
fun build() {
    class Target<T>
    class Local(val value: Alias<Leaf>)
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeferredLocalClassifierHeader")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let parser_class = |expected: &str| {
        analysis.files[0]
            .local_class_enclosing_declarations
            .keys()
            .copied()
            .find(|declaration| {
                matches!(
                    analysis.files[0].decl(*declaration),
                    crate::ast::Decl::Class(class)
                        if class.name.rsplit('.').next() == Some(expected)
                )
            })
            .unwrap_or_else(|| panic!("parser-hoisted {expected} classifier"))
    };
    let parser_shadow = parser_class("Target");
    let parser_local = parser_class("Local");
    let source_id = crate::fir::SourceFileId::from_raw(0);
    let streamed = analysis
        .streamed
        .take()
        .expect("the non-local module headers must finalize");
    assert!(
        !streamed.diagnostic_recovery,
        "an undemanded local classifier must not half-finalize Pass 1"
    );
    let (mut index, _, _, _) = streamed.module.into_parts();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source_id,
        &index,
    )
    .expect("local declarations bind to stable inventory identities");
    let shadow = active
        .canonical_classifier_declaration(parser_shadow, &index)
        .expect("stable local shadow classifier");
    let local = active
        .canonical_classifier_declaration(parser_local, &index)
        .expect("stable Local classifier");
    let constructor = index
        .owned_declaration(local, crate::fir::DeclarationKind::Constructor, 0)
        .expect("stable Local primary constructor");
    let property = active
        .constructor_property_declaration(parser_local, 0)
        .expect("stable Local constructor property");

    assert!(index.classifier_header(shadow).is_none());
    assert!(index.classifier_type_arguments(shadow).is_none());
    assert!(index.classifier_header(local).is_none());
    assert!(index.signature(property).is_none());
    let checked = analysis.types[0]
        .as_ref()
        .expect("Pass 2 checked local declarations");
    let parameter = active
        .constructor_parameter(&analysis.files[0], property)
        .expect("active constructor property parameter");
    assert_eq!(
        (
            checked
                .failed_local_declaration_bindings
                .contains(&property),
            checked.resolved_declaration_type(&parameter.ty).is_some(),
            checked
                .resolved_declaration_spelling(&parameter.ty)
                .is_some(),
        ),
        (false, true, true),
        "the deferred constructor property must retain one checked declaration occurrence"
    );

    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source_id,
        &analysis.symbols,
        checked,
        &mut index,
    )
    .expect("deferred local headers publish atomically from checked facts");

    assert!(index.classifier_header(shadow).is_some());
    assert_eq!(index.classifier_own_type_parameter_count(shadow), Some(1));
    assert_eq!(
        index.classifier_type_arguments(shadow).map(<[_]>::len),
        Some(1)
    );
    assert!(index.classifier_header(local).is_some());

    let signature = index
        .signature(constructor)
        .expect("finalized Local constructor signature");
    let [value] = signature.parameters.as_ref() else {
        panic!("Local has exactly one source constructor parameter")
    };
    let value = value.get();
    assert_eq!(
        value.obj_internal(),
        Some(crate::types::type_name("sample/Target"))
    );
    assert_eq!(value.type_args(), [crate::types::Ty::obj("sample/Leaf")]);
    assert_eq!(
        index
            .signature(property)
            .expect("finalized Local property signature")
            .result
            .get(),
        value
    );
}
