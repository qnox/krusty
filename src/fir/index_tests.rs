use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

use super::*;

#[test]
fn member_source_order_interleaves_functions_and_properties() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "class C {\n    fun before() = 1\n    var value = \"x\"\n    fun after() = value.length\n}\n",
        )
        .with_file_stem("Members")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let class = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.kind == DeclarationKind::Classifier
                        && index.declaration_name(*declaration) == Some("C")
                })
        })
        .expect("class C");
    let order = |name: &str| {
        (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .find(|declaration| {
                index
                    .declaration_header(*declaration)
                    .is_some_and(|header| {
                        header.owner == Some(class)
                            && index.declaration_name(*declaration) == Some(name)
                    })
            })
            .and_then(|declaration| index.source_order(declaration))
            .unwrap_or_else(|| panic!("member {name}"))
    };

    assert!(order("before") < order("value"));
    assert!(order("value") < order("after"));
}

#[test]
fn generated_data_members_follow_every_declared_member() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[
            SourceInput::kotlin("data class D(val x: Int) {\n    fun declared(): Int = x\n}\n")
                .with_file_stem("DataOrder"),
        ],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let class = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("D"))
        .expect("data class D");
    let mut declared_max = 0;
    let mut generated_min = u32::MAX;
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        let Some(header) = index.declaration_header(declaration) else {
            continue;
        };
        if header.owner != Some(class) {
            continue;
        }
        let order = index
            .source_order(declaration)
            .expect("member source order");
        if header.flags.has(DeclarationFlags::COMPILER_GENERATED) {
            generated_min = generated_min.min(order);
        } else {
            declared_max = declared_max.max(order);
        }
    }
    assert!(declared_max < generated_min);
}

#[test]
fn resolved_index_owns_semantic_classifier_graph_without_header_syntax() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "package sample\nopen class Parent<T>\nclass Child : Parent<String>()\nval answer: Int = 42\nfun make(): Child = Child()\nfun String.ext(value: Int): Int = value\n",
        )
        .with_file_stem("Hierarchy")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        assert!(
            index.declaration_header(declaration).is_some(),
            "every stable declaration must retain its semantic header"
        );
    }
    let child = (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(raw as u32);
        let classifier = index.classifier_header(declaration)?;
        (classifier.classifier.segment_ref() == "Child").then_some(classifier)
    });
    let child = child.expect("Child classifier header");
    let superclass = child.superclass.expect("selected superclass").get();
    assert_eq!(
        superclass.obj_internal().map(|name| name.segment_ref()),
        Some("Parent")
    );
    assert_eq!(superclass.type_args(), [crate::types::Ty::String]);

    let make = (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(raw as u32);
        let header = index.declaration_header(declaration)?;
        (header.kind == DeclarationKind::Function)
            .then(|| index.callable_for_declaration(declaration))?
    });
    let make = make.expect("stable function realization");
    assert_eq!(index.callable_name(make.id), Some("make"));
    let extension = (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(raw as u32);
        let callable = index.callable_for_declaration(declaration)?;
        (index.callable_name(callable.id) == Some("ext")).then_some(callable)
    });
    let extension = extension.expect("stable extension realization");
    assert_eq!(
        extension.shape.extension_receiver.map(ResolvedTy::get),
        Some(crate::types::Ty::String)
    );
    assert_eq!(
        index
            .signature(extension.declaration)
            .unwrap()
            .parameters
            .len(),
        1,
        "the extension receiver is a selected receiver, not a value argument"
    );
    assert_eq!(index.callable_parameter_name_count(extension.id), 1);
    assert_eq!(
        index.callable_parameter_name(extension.id, 0),
        Some("value")
    );
    assert!(!index
        .callable_parameter(extension.id, 0)
        .expect("resolved extension parameter facts")
        .flags()
        .is_vararg());
    let answer = (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(raw as u32);
        (index.declaration_header(declaration)?.kind == DeclarationKind::Property)
            .then_some(declaration)
    });
    assert_eq!(
        answer.and_then(|declaration| index.declaration_name(declaration)),
        Some("answer")
    );
    let constructor = (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(raw as u32);
        let header = index.declaration_header(declaration)?;
        (header.kind == DeclarationKind::Constructor)
            .then(|| index.callable_for_declaration(declaration))?
    });
    assert!(matches!(
        constructor.expect("stable constructor realization").name,
        ResolvedCallableName::Constructor
    ));
}

#[test]
fn property_override_edges_are_exact_pending_free_pass_one_facts() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "open class Base<T> { open var value: T? = null }\n\
             class Child : Base<String>() { override var value: String? = null }\n\
             open class Hidden<T> { private val secret: T? = null }\n\
             class Visible : Hidden<String>() { val secret: String? = null }\n\
             interface ResultAny { val result: Any get() = \"Fail\" }\n\
             interface ResultString : ResultAny {\n\
                 override val result: String get() = \"OK\"\n\
             }\n",
        )
        .with_file_stem("Overrides")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let classifier = |name: &str| {
        (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .find(|declaration| {
                index
                    .classifier_header(*declaration)
                    .is_some_and(|header| header.classifier.segment_ref() == name)
            })
            .unwrap_or_else(|| panic!("missing {name} classifier"))
    };

    let [edge] = index.property_overrides(classifier("Child")) else {
        panic!("Child must publish one exact property override")
    };
    assert!(matches!(
        edge.overridden,
        ResolvedPropertyOverrideTarget::Module(_)
    ));
    assert_eq!(edge.overridden_owner.segment_ref(), "Base");
    assert!(!edge.overridden_is_interface);
    assert!(edge.declared_type.get().mentions_ty_param());
    assert_eq!(
        edge.applied_type.get(),
        crate::types::Ty::nullable(crate::types::Ty::String)
    );
    assert_eq!(edge.implementation_type, edge.applied_type);
    assert!(edge.overridden_mutable && edge.implementation_mutable);
    assert!(
        index.property_overrides(classifier("Visible")).is_empty(),
        "same spelling without an override relation must not become a backend bridge candidate"
    );

    let [edge] = index.property_overrides(classifier("ResultString")) else {
        panic!("interface property override must publish its exact parent edge")
    };
    assert!(edge.overridden_is_interface);
    assert_eq!(
        edge.declared_type.get(),
        crate::types::Ty::obj("kotlin/Any")
    );
    assert_eq!(edge.implementation_type.get(), crate::types::Ty::String);
}

#[test]
fn anonymous_object_inferred_override_stays_deferred_until_its_checked_body() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![stdlib, jdk]));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "import kotlin.coroutines.*\n\
             fun make(): Continuation<Any?> = object : Continuation<Any?> {\n\
                 override val context = EmptyCoroutineContext\n\
                 override fun resumeWith(result: Result<Any?>) {}\n\
             }\n",
        )
        .with_file_stem("AnonymousContext")],
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let anonymous = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header
                        .flags
                        .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                })
        })
        .expect("anonymous classifier declaration");
    let context = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.owner == Some(anonymous)
                        && header.kind == crate::fir::DeclarationKind::Property
                        && index.declaration_name(*declaration) == Some("context")
                })
        })
        .expect("anonymous context declaration");
    assert!(index.signature(context).is_none());
    assert!(!index.has_property_override_plan(anonymous));
    assert!(!index.has_function_override_plan(anonymous));
}

#[test]
fn superclass_function_override_edge_precedes_backend_erasure() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "open class Base<T> { open fun choose(value: T): T = value }\n\
             class Child : Base<String>() {\n\
                 override fun choose(value: String): String = value\n\
             }\n",
        )
        .with_file_stem("FunctionOverrides")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let child = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.segment_ref() == "Child")
        })
        .expect("Child classifier");
    let [edge] = index.function_overrides(child) else {
        panic!("Child.choose must publish one exact superclass edge")
    };

    assert!(matches!(
        edge.overridden,
        ResolvedFunctionOverrideTarget::Module(_)
    ));
    assert_eq!(edge.overridden_owner.segment_ref(), "Base");
    assert!(!edge.overridden_is_interface);
    assert!(matches!(
        edge.declared_parameters.as_ref(),
        [parameter]
            if matches!(
                parameter.get(),
                crate::types::Ty::TyParam(_, bound)
                    if *bound == crate::types::Ty::nullable(crate::types::Ty::obj("kotlin/Any"))
            )
    ));
    assert_eq!(
        edge.applied_parameters
            .iter()
            .map(|ty| ty.get())
            .collect::<Vec<_>>(),
        [crate::types::Ty::String]
    );
    assert_eq!(
        edge.implementation_parameters, edge.applied_parameters,
        "backend lowering receives the selected edge; it does not match the overload again"
    );
    assert_eq!(edge.applied_result.get(), crate::types::Ty::String);
    assert_eq!(edge.implementation_result, edge.applied_result);
}

#[test]
fn interface_function_override_edge_precedes_backend_erasure() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "interface Matcher<T> { fun matches(value: T): T }\n\
             class StringMatcher : Matcher<String> {\n\
                 override fun matches(value: String): String = value\n\
             }\n",
        )
        .with_file_stem("InterfaceOverrides")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let implementation = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.segment_ref() == "StringMatcher")
        })
        .expect("StringMatcher classifier");
    let [edge] = index.function_overrides(implementation) else {
        panic!("StringMatcher.matches must publish one exact interface edge")
    };

    assert!(matches!(
        edge.overridden,
        ResolvedFunctionOverrideTarget::Module(_)
    ));
    assert_eq!(edge.overridden_owner.segment_ref(), "Matcher");
    assert!(edge.overridden_is_interface);
    assert!(edge.declared_parameters[0].get().mentions_ty_param());
    assert_eq!(edge.applied_parameters[0].get(), crate::types::Ty::String);
    assert_eq!(edge.implementation_parameters, edge.applied_parameters);
    assert_eq!(edge.declared_result, edge.declared_parameters[0]);
    assert_eq!(edge.applied_result, edge.applied_parameters[0]);
    assert_eq!(edge.implementation_result, edge.applied_result);
}

#[test]
fn enum_entry_override_edge_is_owned_by_the_stable_entry() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "interface Action<T> { fun apply(value: T): String }\n\
             enum class EntryEnum : Action<String> {\n\
                 ONLY { override fun apply(value: String): String = value }\n\
             }\n",
        )
        .with_file_stem("EnumEntryOverrides")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let entry = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| header.kind == DeclarationKind::EnumEntry)
                && index.declaration_name(*declaration) == Some("ONLY")
        })
        .expect("ONLY entry");
    let [edge] = index.function_overrides(entry) else {
        let declarations = (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .filter_map(|declaration| {
                index.declaration_header(declaration).map(|header| {
                    (
                        declaration,
                        header.kind,
                        header.owner,
                        header.flags,
                        index.declaration_name(declaration),
                    )
                })
            })
            .collect::<Vec<_>>();
        panic!(
            "the enum-entry override must publish its exact interface edge; declarations={declarations:?} parent_hierarchy={:?}",
            index
                .declaration_header(entry)
                .and_then(|header| header.owner)
                .and_then(|parent| index.classifier_hierarchy(parent))
        )
    };
    assert_eq!(edge.name.as_ref(), "apply");
    assert!(edge.overridden_is_interface);
    assert_eq!(edge.overridden_owner.segment_ref(), "Action");
    assert!(edge.declared_parameters[0].get().mentions_ty_param());
    assert_eq!(edge.applied_parameters[0].get(), crate::types::Ty::String);
    assert_eq!(
        edge.implementation_parameters[0].get(),
        crate::types::Ty::String
    );
    assert!(edge.implementation_owner.matches("EntryEnum$ONLY"));
}

#[test]
fn inherited_interface_implementation_is_published_before_backend_lowering() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "interface Matcher<T> { fun matches(value: T): T }\n\
             open class StringBase { fun matches(value: String): String = value }\n\
             class StringMatcher : StringBase(), Matcher<String>\n",
        )
        .with_file_stem("InheritedInterfaceOverrides")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let implementation = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.segment_ref() == "StringMatcher")
        })
        .expect("StringMatcher classifier");
    let [edge] = index.function_overrides(implementation) else {
        panic!("StringMatcher must publish its inherited Matcher satisfaction")
    };

    assert!(matches!(
        edge.implementation,
        ResolvedFunctionOverrideTarget::Module(_)
    ));
    assert_eq!(edge.implementation_owner.segment_ref(), "StringBase");
    assert_eq!(edge.overridden_owner.segment_ref(), "Matcher");
    assert!(edge.overridden_is_interface);
    assert_eq!(edge.name.as_ref(), "matches");
    assert_eq!(edge.applied_parameters[0].get(), crate::types::Ty::String);
    assert_eq!(edge.implementation_parameters, edge.applied_parameters);
}

#[test]
fn inherited_interface_property_is_published_before_backend_lowering() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "interface Valued<T> { val value: T }\n\
             open class StringBase { val value: String = \"OK\" }\n\
             class StringValue : StringBase(), Valued<String>\n",
        )
        .with_file_stem("InheritedInterfaceProperties")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let implementation = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.segment_ref() == "StringValue")
        })
        .expect("StringValue classifier");
    let [edge] = index.property_overrides(implementation) else {
        panic!("StringValue must publish its inherited Valued satisfaction")
    };

    assert!(matches!(
        edge.implementation,
        ResolvedPropertyOverrideTarget::Module(_)
    ));
    assert_eq!(edge.implementation_owner.segment_ref(), "StringBase");
    assert_eq!(edge.overridden_owner.segment_ref(), "Valued");
    assert!(edge.overridden_is_interface);
    assert_eq!(edge.name.as_ref(), "value");
    assert_eq!(edge.applied_type.get(), crate::types::Ty::String);
    assert_eq!(edge.implementation_type, edge.applied_type);
}

#[test]
fn mapped_interface_override_publishes_external_declaration_identity() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![stdlib, jdk]));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "abstract class Ints : MutableList<Int> {\n\
                 override fun removeAt(index: Int): Int = index\n\
             }\n",
        )
        .with_file_stem("MappedInterfaceOverride")],
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            classpath.clone(),
        )),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let implementation = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.segment_ref() == "Ints")
        })
        .expect("Ints classifier");
    let edge = index
        .function_overrides(implementation)
        .iter()
        .find(|edge| edge.name.as_ref() == "removeAt")
        .expect("removeAt interface edge");
    let ResolvedFunctionOverrideTarget::External(target) = edge.overridden else {
        panic!("MutableList.removeAt must retain its external declaration identity")
    };
    let realization = classpath
        .external_callable(target)
        .expect("external override target realization");
    assert_eq!(realization.callable.name, "removeAt");
    assert!(realization.callable.owner.matches("java/util/List"));
}

#[test]
fn value_class_override_publishes_the_generic_interface_edge() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![stdlib, jdk]));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "@JvmInline\n\
             value class InlinedComparable<T : Int>(val value: T) :\n\
                 Comparable<InlinedComparable<T>> {\n\
                 override fun compareTo(other: InlinedComparable<T>): Int = 0\n\
             }\n",
        )
        .with_file_stem("ValueClassComparableOverride")],
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            classpath.clone(),
        )),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let implementation = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.segment_ref() == "InlinedComparable")
        })
        .expect("InlinedComparable classifier");
    let edge = index
        .function_overrides(implementation)
        .iter()
        .find(|edge| edge.name.as_ref() == "compareTo")
        .expect("compareTo interface edge");
    assert!(matches!(
        edge.overridden,
        ResolvedFunctionOverrideTarget::External(_)
    ));
    assert!(edge.overridden_is_interface);
    assert_eq!(edge.overridden_owner.segment_ref(), "Comparable");
}
