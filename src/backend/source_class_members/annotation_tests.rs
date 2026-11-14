use super::*;
use crate::types::{type_name, AnnotationRetention};

#[test]
fn source_member_annotation_facts_precede_file_emission() {
    let sources = [
        crate::frontend::SourceInput::kotlin("class Early : Late").with_file_stem("Early"),
        crate::frontend::SourceInput::kotlin(
            r#"
annotation class Mark(val value: String)
@Retention(AnnotationRetention.BINARY)
annotation class BinaryMark
@Retention(AnnotationRetention.SOURCE)
annotation class SourceMark
interface Late {
    @Mark("function") @BinaryMark @SourceMark
    fun value(): String = "value"
    fun plain(): String = "plain"
    val only: String get() = "only"
    @get:Mark("getter") @set:Mark("setter")
    var item: String
        get() = "item"
        set(value) {}
}
"#,
        )
        .with_file_stem("Late"),
    ];
    let analysis = checked_annotation_sources(&sources);
    let index = analysis
        .streamed
        .as_ref()
        .expect("streamed source module")
        .module
        .index();
    let class = index
        .classifier_declaration(type_name("Late"))
        .expect("source interface");
    let member = |name| {
        index
            .owned_declarations(class)
            .iter()
            .copied()
            .find(|&declaration| index.declaration_name(declaration) == Some(name))
            .expect("the exact declared member exists")
    };
    let value = index.callable_for_declaration(member("value")).unwrap().id;
    let plain = index.callable_for_declaration(member("plain")).unwrap().id;
    let property = index.property_for_declaration(member("item")).unwrap();
    let only = index.property_for_declaration(member("only")).unwrap();
    let facts = crate::backend::BackendModuleFacts::from_resolved_index(index).unwrap();
    drop(analysis);
    let annotations = facts.source_member_annotations();
    let function = annotations
        .function(value)
        .expect("a registered source function");
    assert_eq!(
        function
            .iter()
            .map(|application| (application.annotation.internal, application.retention))
            .collect::<Vec<_>>(),
        [
            (type_name("Mark"), AnnotationRetention::Default),
            (type_name("BinaryMark"), AnnotationRetention::Binary)
        ]
    );
    assert!(function.has_source_application());
    assert!(function.declares(type_name("SourceMark")));
    let payload = |annotations: &crate::ir::DeclarationAnnotations| {
        let application = &annotations
            .iter()
            .next()
            .expect("one Mark application")
            .annotation;
        assert_eq!(application.internal, type_name("Mark"));
        let [(name, crate::ir::AnnoValue::Const(crate::ir::IrConst::String(value)))] =
            application.values.as_slice()
        else {
            panic!("the exact Mark argument is a string");
        };
        assert_eq!(name, "value");
        value.clone()
    };
    assert_eq!(
        payload(function),
        crate::kt_string::KtString::from("function")
    );
    let getter = annotations.getter(property).unwrap();
    let setter = annotations.setter(property).unwrap();
    assert_eq!(getter.iter().count(), 1);
    assert_eq!(setter.iter().count(), 1);
    assert_eq!(payload(getter), crate::kt_string::KtString::from("getter"));
    assert_eq!(payload(setter), crate::kt_string::KtString::from("setter"));
    let plain = annotations
        .function(plain)
        .expect("a registered unannotated function");
    assert!(!plain.declares_annotations());
    assert!(annotations
        .getter(only)
        .expect("registered val getter")
        .retains_none());
    assert!(annotations.setter(only).is_none());
    assert!(annotations
        .function(crate::fir::CallableId::from_raw(u32::MAX))
        .is_none());
    assert!(annotations
        .getter(crate::fir::PropertyId::from_raw(u32::MAX))
        .is_none());
    assert!(annotations
        .setter(crate::fir::PropertyId::from_raw(u32::MAX))
        .is_none());
}

#[test]
fn registered_annotation_payload_requires_checked_retention() {
    let index = ResolvedModuleIndex::default();
    let annotation = crate::types::ResolvedAnnotation {
        annotation: type_name("missing/Policy"),
        arguments: Vec::new(),
        facts: Default::default(),
        written: None,
    };
    assert!(matches!(checked_applications(&index, &[annotation]),
        Err(crate::backend::BackendFactError::IncompleteAnnotation(identity)) if identity == type_name("missing/Policy")));
    assert!(checked_applications(&index, &[]).unwrap().retains_none());
}

fn checked_annotation_sources(
    sources: &[crate::frontend::SourceInput<'_>],
) -> crate::frontend::SourceSetAnalysis {
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::jvm_classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        sources,
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
                .expect("JVM provider initialization"),
        )),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    analysis
}

#[test]
fn source_annotation_policies_precede_freezing_in_both_file_orders() {
    for policy_first in [false, true] {
        let mut sources = [
            crate::frontend::SourceInput::kotlin(
                "interface Use { @BinaryMark @SourceMark fun value(): String = \"OK\" }",
            )
            .with_file_stem("Use"),
            crate::frontend::SourceInput::kotlin(
                "@Retention(AnnotationRetention.BINARY) annotation class BinaryMark\n\
                 @Retention(AnnotationRetention.SOURCE) annotation class SourceMark",
            )
            .with_file_stem("Policy"),
        ];
        if policy_first {
            sources.swap(0, 1);
        }
        let analysis = checked_annotation_sources(&sources);
        let index = analysis
            .streamed
            .as_ref()
            .expect("streamed module")
            .module
            .index();
        let class = index
            .classifier_declaration(type_name("Use"))
            .expect("source interface");
        let declaration = index
            .owned_declaration(class, crate::fir::DeclarationKind::Function, 0)
            .expect("the exact source method header");
        let method = index.callable_for_declaration(declaration).unwrap().id;
        let facts = crate::backend::BackendModuleFacts::from_resolved_index(index).unwrap();
        drop(analysis);
        let annotations = facts
            .source_member_annotations()
            .function(method)
            .expect("registered source method");
        assert_eq!(
            annotations
                .iter()
                .map(|application| (application.annotation.internal, application.retention,))
                .collect::<Vec<_>>(),
            [(type_name("BinaryMark"), AnnotationRetention::Binary)],
            "policy_first={policy_first}"
        );
        assert!(annotations.has_source_application());
        assert!(annotations.declares(type_name("SourceMark")));
    }
}
