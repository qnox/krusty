use super::*;

#[test]
fn pass_one_retains_checked_classifier_annotation_values_by_stable_identity() {
    let source = "annotation class Mark(val text: String, val count: Int, val enabled: Boolean)\n\
                  @Mark(count = 3, enabled = true, text = \"kept\")\n\
                  class Subject { fun discardedBody(): String = \"not retained\" }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Annotations")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject declaration");

    assert_eq!(
        index.declaration_applied_annotations(subject),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("Mark"),
            arguments: vec![
                (
                    "text".to_owned(),
                    crate::types::AnnotationValue::string("kept"),
                ),
                ("count".to_owned(), crate::types::AnnotationValue::Int(3)),
                (
                    "enabled".to_owned(),
                    crate::types::AnnotationValue::Boolean(true),
                ),
            ],
            facts: Default::default(),
        }]
    );
}

/// A source annotation class's `@Target` policy comes from its checked application. Entries of
/// `kotlin.annotation.AnnotationTarget` selected through an import are the semantic policy; an
/// entry of another enum leaves the class with NO semantic policy, only kotlinc's diagnostic
/// recovery from the entry it selected.
#[test]
fn checked_target_applications_publish_valid_policies_and_isolate_invalid_recovery() {
    let source = "import kotlin.annotation.AnnotationTarget.PROPERTY_GETTER\n\
                  enum class MyTarget { PROPERTY_GETTER }\n\
                  @Target(PROPERTY_GETTER) annotation class Imported\n\
                  @Target(MyTarget.PROPERTY_GETTER) annotation class Foreign\n";
    let mut diagnostics = DiagSink::new();
    let platform = Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(crate::toolchain::jvm_classpath_jars_for("")),
        ))
        .expect("JVM provider initialization"),
    );
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Targets")],
        crate::frontend::PlatformProvider::jvm(platform),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        ["argument type mismatch: actual type is 'MyTarget', but 'AnnotationTarget' was expected."]
    );
    let policies = analysis
        .streamed
        .as_ref()
        .expect("Pass 1")
        .module
        .index()
        .annotation_targets();
    let getter =
        crate::types::AnnotationTargets::kotlin([crate::types::KotlinTarget::PropertyGetter]);
    let imported = crate::types::type_name("Imported");
    assert_eq!(policies.semantic(imported), Some(getter));
    assert_eq!(policies.diagnostic(imported), Some(getter));
    let foreign = crate::types::type_name("Foreign");
    assert_eq!(policies.semantic(foreign), None);
    assert_eq!(policies.diagnostic(foreign), Some(getter));
}
