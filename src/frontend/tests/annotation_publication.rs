use super::*;

/// Values are published in declaration order, which the class file needs; the order the source
/// wrote them in travels beside them.
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
                written: written("PropertyMark", None, &[0]),
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
                written: written(
                    "Other",
                    Some(crate::types::PropertyAnnotationSite::Property),
                    &[0],
                ),
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
            written: written("PropertyMark", None, &[0]),
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
            written: written(
                "Other",
                Some(crate::types::PropertyAnnotationSite::Property),
                &[0],
            ),
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
            written: Some(Box::new(crate::types::WrittenAnnotation {
                short_name: "Mark".into(),
                use_site: None,
                order: [1, 2, 0].into(),
            })),
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

fn jvm_analysis(source: &str, stem: &str) -> SourceSetAnalysis {
    let mut diagnostics = DiagSink::new();
    let platform = Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(crate::toolchain::jvm_classpath_jars_for(source)),
        ))
        .expect("JVM provider initialization"),
    );
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem(stem)],
        crate::frontend::PlatformProvider::jvm(platform),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    analysis
}

fn written(
    short_name: &str,
    use_site: Option<crate::types::PropertyAnnotationSite>,
    order: &[u16],
) -> Option<Box<crate::types::WrittenAnnotation>> {
    Some(Box::new(crate::types::WrittenAnnotation {
        short_name: short_name.into(),
        use_site,
        order: order.into(),
    }))
}

/// A source annotation class publishes the folded default of each element that has one, as an
/// argument folds: constants evaluated, enum entries and class literals resolved, a nested
/// annotation with its own values.
#[test]
fn pass_one_publishes_folded_annotation_element_defaults() {
    let source = "import kotlin.reflect.KClass\n\
                  enum class Level { LOW, HIGH }\n\
                  annotation class Inner(val name: String = \"inner\")\n\
                  annotation class Defaults(\n\
                      val text: String,\n\
                      val big: Long = 1L shl 40,\n\
                      val small: Byte = 3,\n\
                      val level: Level = Level.LOW,\n\
                      val kind: KClass<*> = String::class,\n\
                      val inner: Inner = Inner(),\n\
                      val names: Array<String> = [\"a\"],\n\
                  )\n\
                  annotation class Plain\n";
    let analysis = jvm_analysis(source, "Defaults");
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let class = |name| {
        index
            .classifier_declaration(crate::types::type_name(name))
            .expect("stable annotation class")
    };
    use crate::types::AnnotationValue;
    assert_eq!(
        index.annotation_element_defaults(class("Defaults")),
        Some(
            &[
                ("big".to_owned(), AnnotationValue::Long(1 << 40)),
                ("small".to_owned(), AnnotationValue::Byte(3)),
                (
                    "level".to_owned(),
                    AnnotationValue::Enum(crate::types::type_name("Level"), "LOW".to_owned()),
                ),
                (
                    "kind".to_owned(),
                    AnnotationValue::Class(crate::types::Ty::obj("kotlin/String")),
                ),
                (
                    "inner".to_owned(),
                    AnnotationValue::Annotation {
                        internal: crate::types::type_name("Inner"),
                        values: Vec::new(),
                        written: written("Inner", None, &[]),
                    },
                ),
                (
                    "names".to_owned(),
                    AnnotationValue::Array(vec![AnnotationValue::string("a")]),
                ),
            ][..]
        )
    );
    assert_eq!(
        index.annotation_element_defaults(class("Inner")),
        Some(&[("name".to_owned(), AnnotationValue::string("inner"))][..])
    );
    assert_eq!(index.annotation_element_defaults(class("Plain")), None);
}

/// Value-parameter annotations are published by parameter position, each with the use-site target
/// it was written with; an omitted vararg element folds to an empty array but is not written.
#[test]
fn pass_one_publishes_value_parameter_annotations_as_written() {
    let source = "annotation class Mark(val text: String = \"\")\n\
                  annotation class Many(vararg val values: String)\n\
                  class Subject(@Mark(\"id\") val id: Int, @param:Many(\"p\") @property:Mark plain: String) {\n\
                      constructor(@Many other: Long) : this(0, \"\")\n\
                      fun act(input: String, @Mark(text = \"x\") @Many(\"a\", \"b\") rest: Int) {}\n\
                  }\n\
                  fun top(@Mark first: Int) {}\n";
    let analysis = jvm_analysis(source, "Parameters");
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject");
    let owned = |kind, sibling| {
        index
            .owned_declaration(subject, kind, sibling)
            .expect("stable member")
    };
    use crate::fir::DeclarationKind;
    use crate::types::{AnnotationValue, PropertyAnnotationSite, ResolvedAnnotation};
    let mark = |text: &str, written| ResolvedAnnotation {
        annotation: crate::types::type_name("Mark"),
        arguments: vec![("text".to_owned(), AnnotationValue::string(text))],
        facts: Default::default(),
        written,
    };
    let many = |values: &[&str], written| ResolvedAnnotation {
        annotation: crate::types::type_name("Many"),
        arguments: vec![(
            "values".to_owned(),
            AnnotationValue::Array(
                values
                    .iter()
                    .map(|value| AnnotationValue::string(value))
                    .collect(),
            ),
        )],
        facts: Default::default(),
        written,
    };
    let primary = owned(DeclarationKind::Constructor, 0);
    assert_eq!(
        index.parameter_applied_annotations(primary, 0),
        [mark("id", written("Mark", None, &[0]))]
    );
    assert_eq!(
        index.parameter_applied_annotations(primary, 1),
        [
            many(
                &["p"],
                written("Many", Some(PropertyAnnotationSite::ValueParameter), &[0])
            ),
            ResolvedAnnotation {
                annotation: crate::types::type_name("Mark"),
                arguments: Vec::new(),
                facts: Default::default(),
                written: written("Mark", Some(PropertyAnnotationSite::Property), &[]),
            },
        ]
    );
    let secondary = owned(DeclarationKind::Constructor, 1);
    assert_eq!(
        index.parameter_applied_annotations(secondary, 0),
        [many(&[], written("Many", None, &[]))]
    );
    let act = owned(DeclarationKind::Function, 0);
    assert!(index.parameter_applied_annotations(act, 0).is_empty());
    assert_eq!(
        index.parameter_applied_annotations(act, 1),
        [
            mark("x", written("Mark", None, &[0])),
            many(&["a", "b"], written("Many", None, &[0])),
        ]
    );
    let top = index
        .source_inventory(crate::fir::SourceFileId::from_raw(0))
        .iter()
        .copied()
        .find(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| {
                    anchor.owner.is_none() && anchor.kind == DeclarationKind::Function
                })
        })
        .expect("stable top-level function");
    assert_eq!(
        index.parameter_applied_annotations(top, 0),
        [ResolvedAnnotation {
            annotation: crate::types::type_name("Mark"),
            arguments: Vec::new(),
            facts: Default::default(),
            written: written("Mark", None, &[]),
        }]
    );
}
