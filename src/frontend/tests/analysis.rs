use super::*;

struct ExistingLibrary;

impl ExistingLibrary {
    fn classifier_internal(
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> Option<&'static str> {
        use crate::symbol_source::SymbolNamespace;
        match namespace {
            SymbolNamespace::Package(package) if package.matches("fixture") => match name {
                "Present" => Some("fixture/Present"),
                "Stable" => Some("fixture/Stable"),
                "Qualified" => Some("fixture/Qualified"),
                "Container" => Some("fixture/Container"),
                "Outer" => Some("fixture/Outer"),
                "CollisionEnum" => Some("fixture/CollisionEnum"),
                _ => None,
            },
            SymbolNamespace::Package(package) if package.matches("support") => match name {
                "BaseScope" => Some("support/BaseScope"),
                "BaseTarget" => Some("support/BaseTarget"),
                "Target" => Some("support/Target"),
                _ => None,
            },
            SymbolNamespace::Classifier(owner) if owner.matches("fixture/Stable") => {
                (name == "Companion").then_some("fixture/Stable$Companion")
            }
            SymbolNamespace::Classifier(owner) if owner.matches("fixture/Qualified") => {
                (name == "Companion").then_some("fixture/Qualified$Companion")
            }
            SymbolNamespace::Classifier(owner) if owner.matches("fixture/Container") => {
                (name == "Labels").then_some("fixture/Container$Labels")
            }
            SymbolNamespace::Classifier(owner) if owner.matches("fixture/Outer") => {
                (name == "Hidden").then_some("fixture/Outer$Hidden")
            }
            SymbolNamespace::Classifier(owner) if owner.matches("fixture/Outer$Hidden") => {
                (name == "Context").then_some("fixture/Outer$Hidden$Context")
            }
            _ => None,
        }
    }

    fn classifier_record(&self, internal: TypeName) -> Option<std::sync::Arc<LibraryType>> {
        let known = [
            "fixture/Present",
            "fixture/Stable",
            "fixture/Stable$Companion",
            "fixture/Qualified",
            "fixture/Qualified$Companion",
            "fixture/Container",
            "fixture/Container$Labels",
            "support/BaseScope",
            "support/BaseTarget",
            "support/Target",
            "fixture/Outer",
            "fixture/Outer$Hidden",
            "fixture/Outer$Hidden$Context",
            "fixture/CollisionEnum",
        ];
        known.iter().any(|name| internal.matches(name)).then(|| {
            let mut supertypes = TypeNameList::new();
            if internal.matches("support/Target") {
                supertypes.push("support/BaseTarget");
            }
            let mut declared_callables = std::collections::HashMap::new();
            if internal.matches("fixture/Container$Labels") {
                let receiver = Ty::obj_name(internal);
                declared_callables.insert(
                    "marker".to_string(),
                    Callables::Properties(PropertySet {
                        overloads: vec![PropertyInfo {
                            name: "marker".to_string(),
                            kind: PropKind::Member,
                            receiver: Some(receiver),
                            formals: Vec::new(),
                            ty: Ty::Int,
                            context_count: 0,
                            context_param_names: Vec::new(),
                            getter: LibraryCallable::library(
                                internal,
                                "getMarker",
                                Vec::new(),
                                Ty::Int,
                                Ty::Int,
                                "()I",
                            ),
                            setter: None,
                            setter_visibility: crate::types::Visibility::Public,
                            is_const: false,
                            compile_time_constant: None,
                            visibility: Visibility::Private,
                            owner: internal,
                            receiver_rank: 0,
                            source_key: None,
                            stable_declaration: None,
                            getter_declaration: None,
                            setter_declaration: None,
                            source_member: None,
                            accessor_derived: false,
                        }],
                    }),
                );
            }
            if internal.matches("fixture/Stable$Companion") {
                let receiver = Ty::obj_name(internal);
                let callable = LibraryCallable::library(
                    internal,
                    "current",
                    Vec::new(),
                    Ty::Int,
                    Ty::Int,
                    "()I",
                );
                declared_callables.insert(
                    "current".to_string(),
                    Callables::Functions(FunctionSet {
                        overloads: vec![FunctionInfo::plain(
                            FnKind::Member,
                            Some(receiver),
                            callable,
                        )],
                    }),
                );
            }
            if internal.matches("fixture/Qualified$Companion") {
                let receiver = Ty::obj_name(internal);
                let callable = LibraryCallable::library(
                    internal,
                    "select",
                    vec![Ty::obj("right/Token")],
                    Ty::Int,
                    Ty::Int,
                    "(Lright/Token;)I",
                );
                declared_callables.insert(
                    "select".to_string(),
                    Callables::Functions(FunctionSet {
                        overloads: vec![FunctionInfo::plain(
                            FnKind::Member,
                            Some(receiver),
                            callable,
                        )],
                    }),
                );
            }
            let companion_object = if internal.matches("fixture/Stable") {
                Some((
                    "Companion".to_string(),
                    crate::types::type_name("fixture/Stable$Companion"),
                ))
            } else if internal.matches("fixture/Qualified") {
                Some((
                    "Companion".to_string(),
                    crate::types::type_name("fixture/Qualified$Companion"),
                ))
            } else {
                None
            };
            std::sync::Arc::new(LibraryType {
                is_kotlin: true,
                access: crate::libraries::ClassifierAccess::Public,
                source_file: None,
                stable_declaration: None,
                is_nested: internal.contains("$"),
                outer_instance: None,
                kind: if internal.matches("fixture/Container$Labels") {
                    TypeKind::Object
                } else {
                    TypeKind::Class
                },
                inheritance: Default::default(),
                supertypes,
                supertype_templates: Vec::new(),
                constructors: Vec::new(),
                hidden_member_properties: Default::default(),
                declared_callables,
                declared_callable_order: Vec::new(),
                members: Vec::new(),
                companion: Vec::new(),
                constants: std::collections::HashMap::new(),
                sam_eligible: false,
                callable_signature: None,
                callable_signatures: Vec::new(),
                companion_object,
                value_underlying: None,
                value_underlying_property: None,
                alias_target: None,
                type_parameters: crate::types::TypeParameters::default(),
                own_type_parameter_count: 0,
                sealed_subclasses: TypeNameList::new(),
                enum_entries: Vec::new(),
                enum_entries_accessor: None,
                named_parameter_lists: Vec::new(),
                retention: None,
                annotation_targets: None,
            })
        })
    }
}

impl crate::symbol_source::SymbolSource for ExistingLibrary {
    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<ResolvedSymbols> {
        let classifier_name =
            Self::classifier_internal(namespace, name).map(crate::types::type_name);
        let classifier = classifier_name.and_then(|internal| self.classifier_record(internal));
        if namespace == crate::symbol_source::SymbolNamespace::Package(crate::types::type_name(""))
            && name == "shadowedProperty"
        {
            let owner = crate::types::type_name("external/LibraryKt");
            let getter = LibraryCallable::library(
                owner,
                "getShadowedProperty",
                Vec::new(),
                Ty::Int,
                Ty::Int,
                "()I",
            );
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Callables::Properties(PropertySet {
                    overloads: vec![PropertyInfo {
                        name: name.to_string(),
                        kind: PropKind::TopLevel,
                        receiver: None,
                        formals: Vec::new(),
                        ty: Ty::Int,
                        context_count: 0,
                        context_param_names: Vec::new(),
                        getter,
                        setter: None,
                        setter_visibility: Visibility::Public,
                        is_const: false,
                        compile_time_constant: None,
                        visibility: Visibility::Public,
                        owner,
                        receiver_rank: 0,
                        source_key: None,
                        stable_declaration: None,
                        getter_declaration: None,
                        setter_declaration: None,
                        source_member: None,
                        accessor_derived: false,
                    }],
                }),
                importable_declaration: true,
                ..ResolvedSymbols::default()
            });
        }
        let Some(name) = (namespace
            == crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("support")))
        .then_some(name)
        .filter(|name| matches!(*name, "adjust" | "configure" | "transform")) else {
            return std::rc::Rc::new(ResolvedSymbols {
                classifier_name: classifier.as_ref().and(classifier_name),
                classifier,
                ..ResolvedSymbols::default()
            });
        };
        let receiver = Ty::obj("support/Target");
        let lambda_receiver = Ty::obj("support/BaseScope");
        let mut value_params = Vec::new();
        if name == "adjust" {
            value_params.push(Ty::Int);
        }
        value_params.push(Ty::fun_with_shape(
            vec![lambda_receiver],
            Ty::Unit,
            0,
            true,
            false,
        ));
        let mut physical_params = vec![receiver];
        physical_params.extend(value_params.iter().copied());
        let callable = LibraryCallable::library(
            "support/SupportKt",
            name,
            physical_params,
            Ty::Unit,
            Ty::Unit,
            "",
        );
        let mut function = FunctionInfo::plain(FnKind::Extension, Some(receiver), callable);
        let lambda_param_types = vec![Vec::new(); value_params.len()];
        let mut lambda_receivers = vec![None; value_params.len()];
        *lambda_receivers.last_mut().unwrap() = Some(lambda_receiver);
        let mut lambda_receiver_params = vec![false; value_params.len()];
        *lambda_receiver_params.last_mut().unwrap() = true;
        function.call_sig = CallSig {
            lambda_param_types,
            lambda_receivers,
            lambda_receiver_params,
            required: value_params.len(),
            ..CallSig::default()
        };
        function.generic_sig = Some(GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: Some(receiver),
            params: value_params,
            ret: Ty::Unit,
            return_policy: Default::default(),
        });
        std::rc::Rc::new(ResolvedSymbols {
            classifier_name: classifier.as_ref().map(|classifier| {
                classifier
                    .alias_target
                    .unwrap_or_else(|| classifier_name.expect("classifier identity"))
            }),
            classifier,
            callables: Callables::Functions(FunctionSet {
                overloads: vec![function],
            }),
            importable_declaration: false,
        })
    }
}

impl SemanticPlatform for ExistingLibrary {
    fn classifier_associated_property(
        &self,
        internal: crate::types::TypeName,
        name: &str,
    ) -> Option<crate::libraries::PropertyInfo> {
        (internal.matches("fixture/CollisionEnum") && name == "ANY").then(|| {
            let ty = Ty::obj_name(internal);
            crate::libraries::PropertyInfo {
                name: name.to_string(),
                kind: crate::libraries::PropKind::TopLevel,
                receiver: None,
                formals: Vec::new(),
                ty,
                context_count: 0,
                context_param_names: Vec::new(),
                getter: crate::libraries::LibraryCallable::library(
                    internal,
                    name,
                    Vec::new(),
                    ty,
                    ty,
                    String::new(),
                ),
                setter: None,
                setter_visibility: crate::types::Visibility::Public,
                is_const: false,
                compile_time_constant: None,
                visibility: crate::types::Visibility::Public,
                owner: internal,
                receiver_rank: 0,
                source_key: None,
                stable_declaration: None,
                getter_declaration: None,
                setter_declaration: None,
                source_member: None,
                accessor_derived: false,
            }
        })
    }
}

#[test]
fn standalone_analysis_accepts_simple_function() {
    let mut diags = DiagSink::new();
    let (_file, syms, info) = analyze_source_standalone("fun box(): String = \"OK\"", &mut diags);
    assert!(!diags.has_errors(), "{:?}", diags.diags);
    assert!(syms.is_some());
    assert!(info.is_some());
}

#[test]
fn current_module_property_wins_over_an_equivalent_dependency_property() {
    let inputs = [SourceInput::kotlin(
        "val shadowedProperty = \"OK\"\nfun box(): String = shadowedProperty",
    )];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some());
}

#[test]
fn standalone_analysis_reports_checker_errors() {
    let mut diags = DiagSink::new();
    let (_file, syms, info) = analyze_source_standalone("fun f(): Int = \"no\"", &mut diags);
    assert!(diags.has_errors());
    assert!(syms.is_some());
    assert!(info.is_some());
}

#[test]
fn checked_prefix_reports_cross_file_conflicting_overloads_and_candidates() {
    let target = "fun namedPair(left: Int, right: String): Int = left\n\
                      fun missingNamedArgument(): Int = namedPair(left = 1)";
    let inputs = [
        SourceInput::kotlin(target),
        SourceInput::kotlin("fun namedPair(left: Int, right: String): String = right"),
        SourceInput::kotlin("fun namedPair(left: Int, right: String): String = right"),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(analysis.types[0].is_some());
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        [
            "conflicting overloads:\n\
                 fun namedPair(left: Int, right: String): String\n\
                 fun namedPair(left: Int, right: String): String",
            "no value passed for parameter 'right'.",
            "none of the following candidates is applicable:\n\n\
                 fun namedPair(left: Int, right: String): Int\n\
                 fun namedPair(left: Int, right: String): String\n\
                 fun namedPair(left: Int, right: String): String",
        ]
    );
    let target_diagnostics = diagnostics
        .diags
        .iter()
        .filter(|diagnostic| diagnostic.file == 0)
        .collect::<Vec<_>>();
    assert_eq!(
        &target[target_diagnostics[0].span.lo as usize..target_diagnostics[0].span.hi as usize],
        "fun namedPair(left: Int, right: String): Int"
    );
    for diagnostic in &target_diagnostics[1..] {
        let editor_span = diagnostic.editor_span.unwrap_or(diagnostic.span);
        assert_eq!(
            &target[editor_span.lo as usize..editor_span.hi as usize],
            "namedPair"
        );
    }
}

#[test]
fn conflicting_top_level_bodies_use_their_own_declared_return_types() {
    let inputs = [
        SourceInput::kotlin("fun choose(value: Int): Int = value"),
        SourceInput::kotlin("fun choose(value: Int): String = \"ok\""),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        inputs.len(),
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        [
            "conflicting overloads:\nfun choose(value: Int): String",
            "conflicting overloads:\nfun choose(value: Int): Int",
        ]
    );
}

#[test]
fn backend_names_and_erasure_do_not_change_frontend_overload_identity() {
    // Kotlin applicability distinguishes nullability. A target may later reject two selected
    // declarations whose physical signatures collide, but that representation check is not a FIR
    // declaration conflict and must not change frontend identity.
    let clash = |sources: &[&str]| {
        // This unit test deliberately uses no platform provider. Supply the annotation as an
        // ordinary source classifier and import it, so the signature pass exercises resolved
        // annotation identity instead of recognizing the spelling `JvmName` intrinsically.
        let mut inputs = vec![SourceInput::kotlin(
            "package kotlin.jvm\nannotation class JvmName(val name: String)",
        )];
        let imported = sources
            .iter()
            .map(|source| format!("import kotlin.jvm.JvmName\n{source}"))
            .collect::<Vec<_>>();
        inputs.extend(imported.iter().map(|source| SourceInput::kotlin(source)));
        let mut diagnostics = DiagSink::new();
        analyze_source_set_prefix_with_features(
            &inputs,
            inputs.len(),
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.msg.starts_with("conflicting overloads:"))
            .count()
    };
    assert_eq!(
        clash(&["fun g(x: String): String = \"nn\"\nfun g(x: String?): String = \"nl\""]),
        0,
    );
    // `@JvmName` is retained for the JVM backend, but does not participate in source lookup.
    assert_eq!(
        clash(&["fun g(x: String): String = \"nn\"\n\
                 @JvmName(\"gNullable\")\n\
                 fun g(x: String?): String = \"nl\""]),
        0,
    );
    // Distinct source names likewise remain distinct even when a backend annotation maps them to
    // the same emitted name. The JVM backend owns reporting that physical collision.
    assert_eq!(
        clash(&["@JvmName(\"same\")\n\
                 fun a(x: Int): String = \"a\"\n\
                 @JvmName(\"same\")\n\
                 fun b(x: Int): String = \"b\""]),
        0,
    );
}

#[test]
fn inferred_conflict_displays_only_source_signature_types() {
    let inputs = [
        SourceInput::kotlin("package sample\nclass Result\nfun choose(value: Int) = Result()"),
        SourceInput::kotlin("package sample\nfun choose(value: Int) = Result()"),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        inputs.len(),
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        [
            "conflicting overloads:\nfun choose(value: Int)",
            "conflicting overloads:\nfun choose(value: Int)",
        ]
    );
}

#[test]
fn mixed_private_public_conflicts_retain_visible_representatives_in_either_order() {
    for public_first in [false, true] {
        let private_declarations = (0..64)
            .map(|index| {
                format!(
                    "private fun crowded(value: Int, required: String): Int = value // {index}\n"
                )
            })
            .collect::<String>();
        let public_declarations = (0..64)
            .map(|index| {
                format!("fun crowded(value: Int, required: String): String = required // {index}\n")
            })
            .collect::<String>();
        let source = if public_first {
            format!(
                "{public_declarations}{private_declarations}\
                     fun use(): Int = crowded(value = 1)"
            )
        } else {
            format!(
                "{private_declarations}{public_declarations}\
                     fun use(): Int = crowded(value = 1)"
            )
        };
        let inputs = [SourceInput::kotlin(&source)];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            inputs.len(),
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let candidate_report = diagnostics
            .diags
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .msg
                    .starts_with("none of the following candidates is applicable:")
            })
            .expect("conflicting call should report its retained candidates");
        assert!(
            candidate_report
                .msg
                .contains("fun crowded(value: Int, required: String): String"),
            "public declaration must survive private candidates when public_first={public_first}"
        );
        assert!(
            candidate_report
                .msg
                .contains("private fun crowded(value: Int, required: String): Int")
                || candidate_report
                    .msg
                    .contains("fun crowded(value: Int, required: String): Int"),
            "private declaration must survive public candidates when public_first={public_first}"
        );
        assert!(candidate_report.msg.lines().skip(2).count() <= 64);
    }
}

#[test]
fn conflicting_overload_diagnostics_sort_candidate_displays_stably() {
    let inputs = [
        SourceInput::kotlin(
            "fun namedPair(left: Int, right: String): String = right\n\
                 fun use(): String = namedPair(left = 1, unknown = 2, right = \"ok\")",
        ),
        SourceInput::kotlin("fun namedPair(left: Int, right: String): String = right"),
        SourceInput::kotlin("fun namedPair(left: Int, right: String): Int = left"),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(
        diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        [
            "conflicting overloads:\n\
                 fun namedPair(left: Int, right: String): Int\n\
                 fun namedPair(left: Int, right: String): String",
            "no parameter with name 'unknown' found.",
            "none of the following candidates is applicable:\n\n\
                 fun namedPair(left: Int, right: String): Int\n\
                 fun namedPair(left: Int, right: String): String\n\
                 fun namedPair(left: Int, right: String): String",
        ]
    );
}

#[test]
fn conflict_recovery_uses_alias_and_qualified_scopes() {
    let target = "package use\n\
                      import a.pick as choose\n\
                      fun aliasUse(): Int = choose(value = 1)\n\
                      fun qualifiedUse(): Int = a.pick(value = 1)";
    let inputs = [
        SourceInput::kotlin(target),
        SourceInput::kotlin("package a\nfun pick(value: Int, other: String): Int = value"),
        SourceInput::kotlin("package a\nfun pick(value: Int, other: String): String = other"),
        SourceInput::kotlin("package b\nfun pick(value: Int, other: String): Boolean = true"),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let messages = diagnostics
        .diags
        .iter()
        .filter(|diagnostic| diagnostic.file == 0)
        .map(|diagnostic| diagnostic.msg.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.starts_with("no value passed"))
            .count(),
        2,
        "target diagnostics: {messages:?}"
    );
    let candidates = messages
        .iter()
        .filter(|message| message.starts_with("none of the following candidates is applicable:"))
        .collect::<Vec<_>>();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|message| {
        message.contains("fun pick(value: Int, other: String): Int")
            && message.contains("fun pick(value: Int, other: String): String")
            && !message.contains("Boolean")
    }));
}

#[test]
fn conflicting_overload_diagnostics_are_deterministic_and_bounded() {
    let sources = (0..70)
        .map(|index| format!("fun crowded(value: Int): Int = value // {index}"))
        .collect::<Vec<_>>();
    let inputs = sources
        .iter()
        .map(|source| SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let conflicts = diagnostics
        .diags
        .iter()
        .filter(|diagnostic| diagnostic.msg.starts_with("conflicting overloads:"))
        .collect::<Vec<_>>();
    assert_eq!(conflicts.len(), 70);
    assert_eq!(
        conflicts
            .iter()
            .map(|diagnostic| diagnostic.file)
            .collect::<Vec<_>>(),
        (0..70).collect::<Vec<_>>()
    );
    assert_eq!(conflicts[0].msg.lines().skip(1).count(), 64);
    assert!(conflicts
        .iter()
        .all(|diagnostic| diagnostic.msg.len() <= 64 * 1024));
    assert!(
        conflicts
            .iter()
            .map(|diagnostic| diagnostic.msg.len())
            .sum::<usize>()
            <= 4 * 1024 * 1024
    );
}

#[test]
fn exhausted_conflict_display_budget_preserves_qualified_call_diagnostics() {
    let parameter = "p".repeat(70 * 1024);
    let declarations = [
        format!("package sample\nfun crowded({parameter}: Int): Int = {parameter}"),
        format!("package sample\nfun crowded({parameter}: Int): String = \"value\""),
    ];
    let inputs = [
        SourceInput::kotlin("package use\nfun use(): Int = sample.crowded(unknown = 1)"),
        SourceInput::kotlin(&declarations[0]),
        SourceInput::kotlin(&declarations[1]),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let target_messages = diagnostics
        .diags
        .iter()
        .filter(|diagnostic| diagnostic.file == 0)
        .map(|diagnostic| diagnostic.msg.as_str())
        .collect::<Vec<_>>();
    assert!(target_messages.contains(&"no parameter with name 'unknown' found."));
    assert!(!target_messages.contains(&"none of the following candidates is applicable:"));
    assert!(!target_messages
        .iter()
        .any(|message| message.starts_with("unresolved reference")));
}

#[test]
fn unrelated_inferred_return_arity_diagnostic_keeps_return_type() {
    let inputs = [SourceInput::kotlin(
        "fun inferred() = 1\nfun use(): Int = inferred(1)",
    )];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        inputs.len(),
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics
        .diags
        .iter()
        .any(|diagnostic| { diagnostic.msg == "too many arguments for 'fun inferred(): Int'." }));
}

#[test]
fn cross_file_private_top_level_function_conflicts_with_public_but_does_not_escape_scope() {
    let target = "fun namedPair(left: Int, right: String): Int = left\n\
                      fun missingNamedArgument(): Int = namedPair(left = 1)";
    let inputs = [
        SourceInput::kotlin(target),
        SourceInput::kotlin("private fun namedPair(left: Int, right: String): String = right"),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(
        diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        ["no value passed for parameter 'right'."]
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 1)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        ["conflicting overloads:\nfun namedPair(left: Int, right: String): Int"]
    );
}

#[test]
fn cross_file_private_top_level_callable_reference_reports_visibility() {
    let inputs = [
        SourceInput::kotlin("val reference: (Int) -> Int = ::hidden"),
        SourceInput::kotlin("private fun hidden(value: Int): Int = value"),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        inputs.len(),
        inputs.len(),
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(
        diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        ["cannot access 'hidden': it is private in its file"]
    );
}

#[test]
fn unavailable_context_does_not_hide_inapplicable_candidate_family() {
    let source = "class Scope\n\
                      context(scope: Scope) fun choose(value: Int): Int = value\n\
                      context(scope: Scope) fun choose(other: Int): String = \"\"\n\
                      fun use(): Int = choose(value = 1)";
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &[SourceInput::kotlin(source)],
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        diagnostics.diags.iter().any(|diagnostic| {
            diagnostic.msg
                == "none of the following candidates is applicable:\n\n\
                        context(scope: Scope) fun choose(other: Int): String\n\
                        context(scope: Scope) fun choose(value: Int): Int"
        }),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn script_analysis_respects_declaration_order() {
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::new(
        SourceKind::KotlinScript,
        "fun read(): Int = value\nval value = 1",
    )];
    analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diags,
    );
    assert!(diags.has_errors());

    let mut diags = DiagSink::new();
    let inputs = [SourceInput::new(
        SourceKind::KotlinScript,
        "val value = 1\nfun read(): Int = value\nread()",
    )];
    analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diags,
    );
    assert!(!diags.has_errors(), "{:?}", diags.diags);
}

#[test]
fn script_declarations_do_not_enter_module_scope() {
    let mut diags = DiagSink::new();
    let inputs = [
        SourceInput::new(
            SourceKind::KotlinScript,
            "fun scriptFunction(): Int = 1\n\
                 class ScriptClass\n\
                 ScriptClass()\n\
                 scriptFunction()",
        ),
        SourceInput::kotlin(
            "fun useFunction(): Int = scriptFunction()\n\
                 fun useClass(): ScriptClass = ScriptClass()",
        ),
        SourceInput::new(
            SourceKind::KotlinScript,
            "class ScriptClass\nval instance = ScriptClass()",
        ),
    ];
    analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diags,
    );

    assert!(diags.diags.iter().any(|diagnostic| diagnostic.file == 1));
    assert!(!diags.diags.iter().any(|diagnostic| diagnostic.file == 0));
    assert!(!diags.diags.iter().any(|diagnostic| diagnostic.file == 2));
}

#[test]
fn script_analysis_rejects_jumps_without_an_enclosing_target() {
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::new(
        SourceKind::KotlinScript,
        "return\nbreak\ncontinue",
    )];
    analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diags,
    );

    assert_eq!(diags.diags.len(), 3);
}

#[test]
fn dependency_symbols_keep_compiled_declarations_and_add_missing_overloads() {
    let features = LangFeatures::new();
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_prefix_with_features(
            &[
                SourceInput::kotlin(
                    "package feature\n\
                     import fixture.Qualified\n\
                     import fixture.Stable\n\
                     import fixture.added\n\
                     import left.Token\n\
                     fun use(): Int = Stable.current() + Stable.current(1) + Qualified.select(Token()) + added(1)",
                ),
                SourceInput::kotlin(
                    "package fixture\nimport left.Token\n\
                     fun added(value: Int): Int = value\n\
                     class Present\n\
                     class Stable { companion object {\n\
                     \u{20} fun current(): String = \"source\"\n\
                     \u{20} fun current(value: Int): Int = value\n\
                     } }\n\
                     class Qualified { companion object {\n\
                     \u{20} fun select(value: Token): Int = 1\n\
                     } }\n\
                     class Added",
                ),
                SourceInput::kotlin("package left\nclass Token"),
            ],
            1,
            1,
            Box::new(ExistingLibrary),
            &features,
            &mut diagnostics,
        );
    let stable = analysis
        .symbols
        .libraries
        .classifier(crate::types::type_name("fixture/Stable$Companion"))
        .expect("compiled and declaration-only companion");
    let stable_current = stable
        .declared_callables
        .get("current")
        .expect("current candidates")
        .functions();
    assert_eq!(
        stable_current
            .iter()
            .map(|candidate| (candidate.semantic_params().to_vec(), candidate.callable.ret))
            .collect::<Vec<_>>(),
        [(Vec::new(), Ty::Int), (vec![Ty::Int], Ty::Int)]
    );
    assert!(
        analysis.types[0].is_some() && diagnostics.diags.is_empty(),
        "diagnostics={:?}, calls={:?}",
        diagnostics.diags,
        analysis.types[0]
            .as_ref()
            .map(|types| &types.resolved_calls)
    );
    let added = analysis.symbols.libraries.symbols(
        crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("fixture")),
        "added",
    );
    let Callables::Functions(functions) = added.callables.clone() else {
        panic!("missing dependency-source function")
    };
    assert_eq!(functions.overloads[0].source_key.map(|key| key.0), Some(1));
}

#[test]
fn dependency_callables_use_compiled_shapes_and_add_source_overloads() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import support.BaseScope\n\
                 import support.Target\n\
                 import support.adjust\n\
                 import support.configure\n\
                 import support.transform\n\
                 object Owner {\n\
                     fun create(target: Target) = target.configure { assign() }\n\
                     fun update(target: Target) = target.transform { assign() }\n\
                     fun change(target: Target) = target.adjust(1) { assign() }\n\
                     private fun BaseScope.assign() {}\n\
                 }",
        ),
        SourceInput::kotlin(
            "package support\n\
                 open class BaseTarget\n\
                 class Target : BaseTarget()\n\
                 open class BaseScope\n\
                 inline fun Target.configure(block: BaseScope.() -> Unit) {}\n\
                 inline fun BaseTarget.transform(block: BaseScope.() -> Unit) {}\n\
                 inline fun Target.adjust(value: String, block: BaseScope.() -> Unit) {}",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        analysis.types[0].is_some() && diagnostics.diags.is_empty(),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn declaration_only_sources_hide_internal_classifiers() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import dependency.Hidden\n\
                 import dependency.Visible\n\
                 fun hidden(): Any = Hidden()\n\
                 fun visible(): Any = Visible()",
        ),
        SourceInput::kotlin(
            "package dependency\n\
                 internal class Hidden\n\
                 class Visible",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics
        .diags
        .iter()
        .any(|diagnostic| diagnostic.msg.contains("'Hidden'")));
    assert!(!diagnostics
        .diags
        .iter()
        .any(|diagnostic| diagnostic.msg.contains("'Visible'")));
}

#[test]
fn declaration_only_extension_calls_resolve_and_type() {
    // An imported extension from a DECLARATION-ONLY dependency file (beyond the inferred
    // prefix): its `Signature` lives in the dependency table behind the platform seam, not in
    // the checked prefix's symbol table, and the call must still resolve — including an
    // omitted defaulted parameter — and type as the declared return, not `Unit`.
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import dependency.render\n\
                 import dependency.tag\n\
                 class C {\n\
                 \u{20} fun go(): Int {\n\
                 \u{20}\u{20} val r = build()\n\
                 \u{20}\u{20} if (r == null) { return 0 }\n\
                 \u{20}\u{20} return r.length\n\
                 \u{20} }\n\
                 \u{20} fun build() = \"x\".tag()?.render()\n\
                 }",
        ),
        SourceInput::kotlin(
            "package dependency\n\
                 fun String?.tag(): String? = this\n\
                 fun String.render(prefix: (String) -> String = { it }): String = prefix(this)",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        diagnostics.diags.iter().all(|d| d.file != 0),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn modifier_prefixed_local_functions_parse_in_bodies() {
    // `tailrec fun`/`suspend fun` LOCAL declarations are statements in any body, not just
    // scripts — the soft-keyword prefix must not parse as an expression name.
    let inputs = [SourceInput::kotlin(
        "fun outer(n: Int): Int {\n\
             \u{20} tailrec fun down(k: Int): Int = if (k <= 0) 0 else down(k - 1)\n\
             \u{20} return down(n)\n\
             }",
    )];
    let mut diagnostics = DiagSink::new();
    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn local_suspend_functions_reach_semantic_analysis() {
    // Local suspension is part of the function signature. Whether a backend can CPS-lower the
    // lifted local body must not make otherwise valid Kotlin fail in parsing.
    let inputs = [SourceInput::kotlin(
        "fun outer() {\n\
             \u{20} suspend fun inner() {}\n\
             \u{20} println(\"x\")\n\
             }",
    )];
    let mut diagnostics = DiagSink::new();
    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn declaration_only_source_exposes_qualified_nested_enum_entry() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import dependency.Model\n\
                 val context: Model.Context = Model.Context.ANY",
        ),
        SourceInput::kotlin(
            "package dependency\n\
                 class Model { enum class Context { ANY } }",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn declaration_only_source_hides_public_nested_enum_of_internal_class() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import dependency.Hidden.Context\n\
                 val context: Any = Context.ANY",
        ),
        SourceInput::kotlin(
            "package dependency\n\
                 internal class Hidden { enum class Context { ANY } }",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        diagnostics.diags.iter().any(|diagnostic| diagnostic
            .msg
            .contains("cannot access 'Context': it is internal")),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn declaration_only_source_hides_public_enum_below_internal_nested_class() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import dependency.Outer\n\
                 val context: Outer.Hidden.Context? = null",
        ),
        SourceInput::kotlin(
            "package dependency\n\
                 class Outer { internal class Hidden { enum class Context { ANY } } }",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        diagnostics.diags.iter().any(|diagnostic| {
            diagnostic
                .msg
                .contains("cannot access 'Outer.Hidden.Context': it is internal")
        }),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn declaration_only_internal_class_shadows_public_platform_type() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import fixture.*\n\
                 val hidden: Present? = null",
        ),
        SourceInput::kotlin("package fixture\ninternal class Present"),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.iter().any(|diagnostic| diagnostic
        .msg
        .contains("cannot access 'Present': it is internal")));
    assert!(analysis
        .symbols
        .libraries
        .symbols(
            crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("fixture")),
            "Present",
        )
        .classifier
        .as_ref()
        .is_some_and(
            |classifier| classifier.access == crate::libraries::ClassifierAccess::Internal
        ));
}

#[test]
fn declaration_only_internal_nested_class_shadows_public_platform_path() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import fixture.Outer\n\
                 val hidden: Outer.Hidden.Context? = null",
        ),
        SourceInput::kotlin(
            "package fixture\n\
                 class Outer { internal class Hidden { class Context } }",
        ),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.iter().any(|diagnostic| {
        diagnostic
            .msg
            .contains("cannot access 'Outer.Hidden.Context': it is internal")
    }));
    assert!(analysis
        .symbols
        .libraries
        .symbols(
            crate::symbol_source::SymbolNamespace::Classifier(crate::types::type_name(
                "fixture/Outer$Hidden",
            )),
            "Context",
        )
        .classifier
        .as_ref()
        .is_some_and(
            |classifier| classifier.access == crate::libraries::ClassifierAccess::Internal
        ));
    // Every classifier API must report the enclosing source restriction. Returning the public
    // leaf visibility here would let the resolver's public fast path disagree with the type and
    // package-access queries above.
    assert_eq!(
        analysis
            .symbols
            .libraries
            .classifier(crate::types::type_name("fixture/Outer$Hidden$Context"))
            .map(|classifier| classifier.access.visibility()),
        Some(Visibility::Internal)
    );
    assert_eq!(
        analysis
            .symbols
            .libraries
            .classifier(crate::types::type_name("fixture/Outer$Hidden$Context"))
            .map(|classifier| classifier.access),
        Some(crate::libraries::ClassifierAccess::Internal)
    );
}

#[test]
fn declaration_only_internal_ancestor_shadows_absent_platform_descendant() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import fixture.Outer\n\
                 val hidden: Outer.Hidden.Context? = null",
        ),
        SourceInput::kotlin("package fixture\nclass Outer { internal class Hidden }"),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    let hidden = crate::types::type_name("fixture/Outer$Hidden$Context");

    assert!(
        diagnostics.diags.iter().any(|diagnostic| {
            diagnostic
                .msg
                .contains("cannot access 'Outer.Hidden.Context': it is internal")
        }),
        "{:?}",
        diagnostics.diags
    );
    assert!(analysis
        .symbols
        .libraries
        .classifier(hidden)
        .is_some_and(
            |classifier| classifier.access == crate::libraries::ClassifierAccess::Internal
        ));
    assert!(analysis
        .symbols
        .libraries
        .symbols(
            crate::symbol_source::SymbolNamespace::Classifier(crate::types::type_name(
                "fixture/Outer$Hidden",
            )),
            "Context",
        )
        .classifier
        .as_ref()
        .is_some_and(
            |classifier| classifier.access == crate::libraries::ClassifierAccess::Internal
        ));
    // Although the leaf exists only on the platform, its source-declared internal owner claims
    // the path. The visibility/access APIs must carry that owner restriction instead of falling
    // through to the platform leaf and describing the same rejected type as public.
    assert_eq!(
        analysis
            .symbols
            .libraries
            .classifier(hidden)
            .map(|classifier| classifier.access.visibility()),
        Some(Visibility::Internal)
    );
    assert_eq!(
        analysis
            .symbols
            .libraries
            .classifier(hidden)
            .map(|classifier| classifier.access),
        Some(crate::libraries::ClassifierAccess::Internal)
    );
}

#[test]
fn declaration_only_public_ancestors_allow_absent_platform_descendant() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import fixture.Outer\n\
                 val visible: Outer.Hidden.Context? = null",
        ),
        SourceInput::kotlin("package fixture\nclass Outer { class Hidden }"),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    let visible = crate::types::type_name("fixture/Outer$Hidden$Context");

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(analysis.symbols.libraries.classifier(visible).is_some());
}

#[test]
fn declaration_only_internal_class_shadows_platform_associated_property() {
    let inputs = [
        SourceInput::kotlin("package consumer\nval checked = Unit"),
        SourceInput::kotlin("package fixture\ninternal class CollisionEnum"),
    ];
    let mut diagnostics = DiagSink::new();

    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    let collision = crate::types::type_name("fixture/CollisionEnum");

    assert_eq!(
        analysis
            .symbols
            .libraries
            .classifier(collision)
            .map(|classifier| classifier.access.visibility()),
        Some(Visibility::Internal)
    );
    assert!(analysis
        .symbols
        .libraries
        .classifier_associated_property(collision, "ANY")
        .is_none());
}

#[test]
fn inferred_friend_sources_expose_internal_classifiers() {
    let inputs = [
        SourceInput::kotlin(
            "package consumer\n\
                 import dependency.Hidden\n\
                 fun hidden(): Any = Hidden()",
        ),
        SourceInput::kotlin("package dependency\ninternal class Hidden"),
    ];
    let mut diagnostics = DiagSink::new();

    analyze_source_set_prefix_with_features(
        &inputs,
        1,
        2,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn dependency_symbols_expose_inherited_nested_classifier_to_subclass() {
    let features = LangFeatures::new();
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_prefix_with_features(
        &[
            SourceInput::kotlin(
                "package consumer\n\
                     import support.Parent\n\
                     class Child(category: Category) : Parent()",
            ),
            SourceInput::kotlin(
                "package support\n\
                     open class Parent { enum class Category { FIRST } }",
            ),
        ],
        1,
        1,
        Box::new(EmptySymbolSource),
        &features,
        &mut diagnostics,
    );

    assert!(
        analysis.types[0].is_some() && diagnostics.diags.is_empty(),
        "{:?}",
        diagnostics.diags
    );
    assert_eq!(
        analysis
            .symbols
            .classes
            .get(&crate::types::type_name("consumer/Child"))
            .expect("consumer class")
            .ctor_params,
        [Ty::obj("support/Parent$Category")]
    );
}

#[test]
fn dependency_symbols_preserve_protected_classifier_for_subclass_only() {
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_prefix_with_features(
        &[
            SourceInput::kotlin(
                "package consumer\n\
                     import support.Parent\n\
                     class Child : Parent() {\n\
                         fun String.read(): String =\n\
                             Category(\"O\").value() + Category(second = \"K\").value()\n\
                         fun value(): String = \"\".read()\n\
                     }",
            ),
            SourceInput::kotlin(
                "package support\n\
                     open class Parent {\n\
                         protected class Category(\n\
                             private val first: String = \"O\",\n\
                             private val second: String = \"K\",\n\
                         ) { fun value(): String = first + second }\n\
                     }",
            ),
        ],
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        analysis.types[0].is_some() && diagnostics.diags.is_empty(),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn dependency_symbols_do_not_globally_expose_protected_classifier() {
    let mut diagnostics = DiagSink::new();
    analyze_source_set_prefix_with_features(
        &[
            SourceInput::kotlin(
                "package consumer\n\
                     import support.Parent\n\
                     class Unrelated { fun make(): Any = Category() }",
            ),
            SourceInput::kotlin(
                "package support\n\
                     open class Parent { protected class Category }",
            ),
        ],
        1,
        1,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("Category")),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn dependency_symbols_do_not_expose_nested_classifier_outside_subclass() {
    let features = LangFeatures::new();
    let mut diagnostics = DiagSink::new();
    analyze_source_set_prefix_with_features(
        &[
            SourceInput::kotlin(
                "package consumer\n\
                     import support.Parent\n\
                     class Unrelated(category: Category)",
            ),
            SourceInput::kotlin(
                "package support\n\
                     open class Parent { enum class Category { FIRST } }",
            ),
        ],
        1,
        1,
        Box::new(EmptySymbolSource),
        &features,
        &mut diagnostics,
    );

    assert!(
        diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("unresolved reference 'Category'")),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn dependency_symbols_add_a_source_property_missing_from_the_public_api() {
    let features = LangFeatures::new();
    let inputs = [
        SourceInput::kotlin(
            "package feature\n\
                 import fixture.Container\n\
                 fun use(): Int = Container.Labels.marker",
        ),
        SourceInput::kotlin(
            "package fixture\n\
                 class Container {\n\
                     object Labels { val marker: Int = 1 }\n\
                 }",
        ),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        Box::new(ExistingLibrary),
        &features,
        &mut diagnostics,
    );
    assert!(
        analysis.types[0].is_some() && diagnostics.diags.is_empty(),
        "{:?}",
        diagnostics.diags
    );
}

#[test]
fn source_set_analysis_applies_multiplatform_actualization() {
    let source = "// LANGUAGE: +MultiPlatformProjects\n\
                      expect fun value(): String\n\
                      actual fun value(): String = \"OK\"\n\
                      fun box(): String = value()";
    let inputs = [SourceInput::kotlin(source)];
    let mut diags = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diags,
    );
    assert!(!diags.has_errors(), "{:?}", diags.diags);
    assert!(analysis.types[0].is_some());
}

#[test]
fn frontend_keeps_nullability_distinct_overloads_after_actualization() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect fun o(x: String?): String?\n\
             expect fun k(x: String?): String?\n\
             fun box(): Unit { o(null); k(null) }\n",
        )
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "fun o(x: String): String = \"\"\n\
             actual fun o(x: String?): String? = \"O\"\n\
             actual fun k(x: String?): String? = \"K\"\n\
             fun k(x: String): String = \"\"\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);

    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn preexisting_warning_does_not_mark_a_source_as_unparseable() {
    let mut diags = DiagSink::new();
    diags.diags.push(Diagnostic {
        span: Span::new(0, 0),
        editor_span: None,
        severity: Severity::Warning,
        kind: crate::diag::DiagnosticKind::Compiler,
        msg: "existing warning".to_string(),
        identity: None,
        file: 0,
    });
    let inputs = [SourceInput::kotlin("fun value(): Int = 1")];
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diags,
    );
    assert!(analysis.types[0].is_some());
}
