use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

#[test]
fn local_classifier_and_publishable_constructor_property_finalize_in_pass_one() {
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

    assert!(index.classifier_header(shadow).is_some());
    assert_eq!(index.classifier_own_type_parameter_count(shadow), Some(1));
    assert_eq!(
        index.classifier_type_arguments(shadow).map(<[_]>::len),
        Some(1)
    );
    assert!(index.classifier_header(local).is_some());
    let pass_one_property = index
        .signature(property)
        .expect("representable constructor property is a Pass-1 contract")
        .result
        .get();
    assert!(index.signature(constructor).is_some());
    assert!(index.declaration_spellings_deferred(constructor));
    assert!(index.declaration_spellings(constructor).is_none());
    assert!(index.declaration_spellings_deferred(property));
    assert!(index.declaration_spellings(property).is_none());
    assert!(index
        .declaration_spellings(local)
        .expect("classifier spelling roles outside the primary parameter publish in Pass 1")
        .params
        .is_empty());
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
        "the published constructor property retains its checked declaration occurrence"
    );

    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source_id,
        &analysis.symbols,
        checked,
        &mut index,
    )
    .expect("remaining deferred local headers publish from checked facts");

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
        pass_one_property
    );
    assert_eq!(pass_one_property, value);
    for declaration in [constructor, property] {
        assert!(!index.declaration_spellings_deferred(declaration));
    }
    let constructor_spelling = index
        .declaration_spellings(constructor)
        .expect("constructor spelling publication")
        .param(0);
    let property_spelling = &index
        .declaration_spellings(property)
        .expect("constructor-property spelling publication")
        .ret;
    let classifier_spelling = index
        .declaration_spellings(local)
        .expect("classifier primary-parameter spelling completion")
        .param(0);
    assert_eq!(
        constructor_spelling.alias,
        Some(crate::types::type_name("sample/Alias"))
    );
    assert_eq!(constructor_spelling, property_spelling);
    assert_eq!(constructor_spelling, classifier_spelling);
}

#[test]
fn mandatory_local_default_reuses_selected_and_secondary_binds_active_spelling() {
    let source = r#"
package sample

typealias Text = String

fun build() {
    class Local {
        constructor(value: Text)

        fun echo(value: Text = "OK"): Text = value
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("MandatoryLocalSpellings")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let source_id = crate::fir::SourceFileId::from_raw(0);
    let parser_local = analysis.files[0]
        .local_class_enclosing_declarations
        .keys()
        .copied()
        .find(|declaration| {
            matches!(
                analysis.files[0].decl(*declaration),
                crate::ast::Decl::Class(class)
                    if class.name.rsplit('.').next() == Some("Local")
            )
        })
        .expect("parser local classifier");
    let streamed = analysis.streamed.take().expect("Pass-1 module");
    assert!(!streamed.diagnostic_recovery);
    let (mut index, _, _, _) = streamed.module.into_parts();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source_id,
        &index,
    )
    .expect("stable local declaration inventory");
    let local = active
        .canonical_classifier_declaration(parser_local, &index)
        .expect("stable local classifier");
    let secondary = index
        .owned_declaration(local, crate::fir::DeclarationKind::Constructor, 1)
        .expect("stable secondary constructor");
    let method = active
        .source_member_declaration(
            &analysis.files[0],
            &index,
            crate::libraries::SourceMember::Class {
                file: 0,
                owner: parser_local.0,
                method: 0,
            },
        )
        .expect("stable defaulted method");

    assert!(index.declaration_semantic_header_complete(secondary));
    assert!(index.declaration_spellings_deferred(secondary));
    assert!(index.declaration_spellings(secondary).is_none());
    assert!(index.declaration_semantic_header_complete(method));
    assert!(!index.declaration_spellings_deferred(method));
    assert!(index.declaration_spellings(method).is_some());

    crate::resolve::publish_checked_default_local_signatures(
        &analysis.files[0],
        &active,
        source_id,
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass-2 checked local declarations"),
        &mut index,
        &[method],
    )
    .expect("deferred spelling publication");

    for declaration in [secondary, method] {
        assert!(!index.declaration_spellings_deferred(declaration));
    }
    let secondary_spelling = index
        .declaration_spellings(secondary)
        .expect("secondary-constructor spellings")
        .param(0);
    let method_spellings = index
        .declaration_spellings(method)
        .expect("defaulted-method spellings");
    assert_eq!(
        secondary_spelling.alias,
        Some(crate::types::type_name("sample/Text"))
    );
    assert_eq!(method_spellings.param(0), secondary_spelling);
    assert_eq!(&method_spellings.ret, secondary_spelling);
}

#[test]
fn mandatory_local_inline_selector_publishes_only_explicitly_deferred_spellings() {
    let source = r#"
package sample

@Target(AnnotationTarget.TYPE)
@Retention(AnnotationRetention.BINARY)
annotation class Bin

typealias Text = String

fun build() {
    class Local {
        inline fun echo(value: (named: @Bin Text) -> Text): Text = value("")
        fun idle(value: @Bin Text) = ""
    }
}
"#;
    let annotation_builtins = r#"
// ALLOW_KOTLIN_PACKAGE
package kotlin.annotation

enum class AnnotationTarget { TYPE }
enum class AnnotationRetention { BINARY }
annotation class Target(vararg val allowedTargets: AnnotationTarget)
annotation class Retention(val value: AnnotationRetention)
"#;
    let inputs = [
        SourceInput::kotlin(source).with_file_stem("MandatoryInlineSpellings"),
        SourceInput::kotlin(annotation_builtins).with_file_stem("AnnotationBuiltins"),
    ];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let source_id = crate::fir::SourceFileId::from_raw(0);
    let parser_local = analysis.files[0]
        .local_class_enclosing_declarations
        .keys()
        .copied()
        .find(|declaration| {
            matches!(
                analysis.files[0].decl(*declaration),
                crate::ast::Decl::Class(class)
                    if class.name.rsplit('.').next() == Some("Local")
            )
        })
        .expect("parser local classifier");
    let streamed = analysis.streamed.take().expect("Pass-1 module");
    assert!(!streamed.diagnostic_recovery);
    let (mut index, _, _, _) = streamed.module.into_parts();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source_id,
        &index,
    )
    .expect("stable local declaration inventory");
    let method = active
        .source_member_declaration(
            &analysis.files[0],
            &index,
            crate::libraries::SourceMember::Class {
                file: 0,
                owner: parser_local.0,
                method: 0,
            },
        )
        .expect("stable inline method");
    let idle = active
        .source_member_declaration(
            &analysis.files[0],
            &index,
            crate::libraries::SourceMember::Class {
                file: 0,
                owner: parser_local.0,
                method: 1,
            },
        )
        .expect("stable whole-deferred method");
    assert!(index.declaration_semantic_header_complete(method));
    assert!(index.declaration_spellings_deferred(method));
    assert!(index.declaration_spellings(method).is_none());
    let deferred = index
        .deferred_declaration_spellings(method)
        .expect("explicit deferred-spelling contract");
    let crate::spelling::DeferredSpellingRole::Active(parameter_facts) = &deferred.params[0] else {
        panic!("the inline parameter's written type belongs to active Pass 2")
    };
    fn assert_checked_tree(spelling: &crate::spelling::Spelled) {
        assert!(spelling
            .annotations
            .iter()
            .chain(&spelling.expansion_annotations)
            .all(|annotation| matches!(
                annotation,
                crate::spelling::TypeUseAnnotation::Checked(_)
            )));
        for (_, argument) in &spelling.alias_args {
            assert_checked_tree(argument);
        }
        for argument in &spelling.args {
            assert_checked_tree(argument);
        }
    }
    assert_checked_tree(parameter_facts);
    assert_eq!(parameter_facts.args.len(), 2);
    assert_eq!(
        parameter_facts.args[0].parameter_name.as_deref(),
        Some("named")
    );
    assert_eq!(parameter_facts.args[0].annotations.len(), 1);
    assert_eq!(
        parameter_facts.args[0].annotations[0].checked().annotation,
        crate::types::type_name("sample/Bin")
    );
    assert!(!index.declaration_semantic_header_complete(idle));
    let idle_deferred = index
        .deferred_declaration_spellings(idle)
        .expect("whole-deferred spelling contract");
    let crate::spelling::DeferredSpellingRole::Active(idle_parameter) = &idle_deferred.params[0]
    else {
        panic!("the unevaluated whole-deferred parameter must remain active")
    };
    assert_checked_tree(idle_parameter);

    crate::resolve::publish_checked_inline_local_signatures(
        &analysis.files[0],
        source_id,
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass-2 checked local declarations"),
        &mut index,
        &[method],
        Some(&active),
    )
    .expect("inline deferred-spelling publication");

    assert!(!index.declaration_spellings_deferred(method));
    let spellings = index
        .declaration_spellings(method)
        .expect("inline method spellings");
    let parameter = spellings.param(0);
    assert_checked_tree(parameter);
    assert_eq!(parameter.args.len(), 2);
    assert_eq!(parameter.args[0].parameter_name.as_deref(), Some("named"));
    assert_eq!(
        parameter.args[0].alias,
        Some(crate::types::type_name("sample/Text"))
    );
    assert_eq!(parameter.args[0].annotations.len(), 1);
    assert_eq!(
        parameter.args[0].annotations[0].checked().annotation,
        crate::types::type_name("sample/Bin")
    );
    assert_eq!(
        parameter.args[1].alias,
        Some(crate::types::type_name("sample/Text"))
    );
    assert_eq!(
        spellings.ret.alias,
        Some(crate::types::type_name("sample/Text"))
    );
    assert!(index.declaration_spellings_deferred(idle));

    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source_id,
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass-2 checked local declarations"),
        &mut index,
    )
    .expect("whole-deferred method publication");
    let idle_spellings = index
        .declaration_spellings(idle)
        .expect("whole-deferred method spellings");
    assert_checked_tree(idle_spellings.param(0));
    assert_eq!(
        idle_spellings.param(0).alias,
        Some(crate::types::type_name("sample/Text"))
    );
    assert_eq!(idle_spellings.param(0).annotations.len(), 1);
}
