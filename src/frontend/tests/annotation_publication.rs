use super::*;

#[test]
fn constructor_property_payloads_do_not_depend_on_annotation_policy_file_order() {
    const POLICY: &str = r#"
import kotlin.reflect.KClass
@Target(AnnotationTarget.PROPERTY)
annotation class PropertyMark(val type: KClass<*>)
@Target(AnnotationTarget.PROPERTY, AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.PROPERTY_GETTER)
annotation class Other(val text: String)
@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class ParamOnly(val text: String)
class Token
"#;
    const USE: &str = r#"
class Subject(
    @PropertyMark(Token::class) @param:Other("parameter") @get:Other("getter") val first: Int,
    @ParamOnly("default parameter") val second: Int,
    @property:Other("property") val third: Int
)
"#;
    for sources in [[USE, POLICY], [POLICY, USE]] {
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            crate::toolchain::jvm_classpath_jars_for(POLICY),
        ));
        let mut diagnostics = DiagSink::new();
        let inputs = sources.map(SourceInput::kotlin);
        let analysis = analyze_source_set_with_features(
            &inputs,
            crate::frontend::PlatformProvider::jvm(Box::new(
                crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
                    .expect("JVM provider initialization"),
            )),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
        let subject = index
            .classifier_declaration(crate::types::type_name("Subject"))
            .expect("stable subject declaration");
        let applications = |sibling| {
            let property = index
                .owned_declaration(subject, crate::fir::DeclarationKind::Property, sibling)
                .expect("stable constructor property");
            index.declaration_applied_annotations(property)
        };
        assert_eq!(
            applications(0),
            [crate::types::ResolvedAnnotation {
                annotation: crate::types::type_name("PropertyMark"),
                arguments: vec![(
                    "type".to_owned(),
                    crate::types::AnnotationValue::Class(Ty::obj("Token"))
                )],
                facts: Default::default(),
            }]
        );
        assert!(applications(1).is_empty());
        assert_eq!(
            applications(2),
            [crate::types::ResolvedAnnotation {
                annotation: crate::types::type_name("Other"),
                arguments: vec![(
                    "text".to_owned(),
                    crate::types::AnnotationValue::string("property")
                )],
                facts: Default::default(),
            }]
        );
    }
}

#[test]
fn pass_one_retains_constructor_property_payloads_only_at_their_checked_use_site() {
    let source = r#"
import kotlin.reflect.KClass
@Target(AnnotationTarget.PROPERTY)
annotation class PropertyMark(val type: KClass<*>)
@Target(AnnotationTarget.PROPERTY, AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.PROPERTY_GETTER)
annotation class Other(val text: String)
class Token
class Subject(
    @PropertyMark(Token::class) @param:Other("parameter") @get:Other("getter") val first: Int,
    @Other("default parameter") val second: Int,
    @property:Other("property") val third: Int
)
"#;
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::jvm_classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("ConstructorPropertyAnnotations")],
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
                .expect("JVM provider initialization"),
        )),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject declaration");
    let applications = |sibling| {
        let property = index
            .owned_declaration(subject, crate::fir::DeclarationKind::Property, sibling)
            .expect("stable constructor property");
        index.declaration_applied_annotations(property)
    };
    assert_eq!(
        applications(0),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("PropertyMark"),
            arguments: vec![(
                "type".to_owned(),
                crate::types::AnnotationValue::Class(Ty::obj("Token"))
            )],
            facts: Default::default(),
        }]
    );
    assert!(applications(1).is_empty());
    assert_eq!(
        applications(2),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("Other"),
            arguments: vec![(
                "text".to_owned(),
                crate::types::AnnotationValue::string("property")
            )],
            facts: Default::default(),
        }]
    );
}

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

#[test]
fn pass_one_publishes_body_local_property_and_accessor_annotations_by_stable_identity() {
    let source = r#"
fun build() {
    class Local {
        @Deprecated("gone", level = DeprecationLevel.HIDDEN)
        val whole: Int = 1

        val read: Int
            @Deprecated("gone", level = DeprecationLevel.HIDDEN)
            get() = 1

        var write: Int = 1
            get() = field
            @Deprecated("gone", level = DeprecationLevel.HIDDEN)
            set(value) { field = value }
    }
}
"#;
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::jvm_classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("BodyLocalAnnotations")],
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
                .expect("JVM provider initialization"),
        )),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
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
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        crate::fir::SourceFileId::from_raw(0),
        index,
    )
    .expect("local declarations bind to their stable inventory");
    let local = active
        .canonical_classifier_declaration(parser_local, index)
        .expect("stable local classifier");
    let property = |sibling| {
        index
            .owned_declaration(local, crate::fir::DeclarationKind::Property, sibling)
            .expect("stable local property")
    };
    let whole = property(0);
    let read = property(1);
    let write = property(2);
    let read_getter = index
        .owned_declaration(read, crate::fir::DeclarationKind::Accessor, 0)
        .expect("stable read getter");
    let write_setter = index
        .owned_declaration(write, crate::fir::DeclarationKind::Accessor, 1)
        .expect("stable write setter");
    for declaration in [whole, read_getter, write_setter] {
        let annotations = index.declaration_applied_annotations(declaration);
        assert_eq!(annotations.len(), 1);
        assert!(annotations[0].is_deprecated_hidden());
    }
    assert!(index.declaration_applied_annotations(read).is_empty());
    assert!(index.declaration_applied_annotations(write).is_empty());
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
