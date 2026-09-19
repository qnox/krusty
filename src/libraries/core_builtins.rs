//! The declarations a target still has when no stdlib artifact is on its classpath.
//!
//! `Any`, `String`, `Enum`, `Function`, `KProperty` and the handful of callables beside them are
//! defined by the LANGUAGE: source that mentions them is valid with an empty classpath, so one
//! symbol source has to answer for them. This module is that source's contents, kept apart from
//! the library model it publishes into.

use super::*;

impl EmptySymbolSource {
    fn builtin_classifier(name: &str, internal: TypeName) -> Option<LibraryType> {
        let known = Ty::from_name(name).is_some()
            || Ty::primitive_array_element(name).is_some()
            || matches!(
                name,
                "Array" | "Enum" | "Function" | "EqualityBound" | "Suppress"
            );
        if !known {
            return None;
        }

        let mut classifier = LibraryType::declaration_header();
        if name == "Function" {
            classifier.kind = TypeKind::Interface;
            classifier.type_parameters =
                TypeParameters::invariant(vec!["R".to_string()], vec![Vec::new()]);
        }
        if name == "Enum" {
            // `enum class E` has the implicit semantic supertype `Enum<E>` even with an empty
            // target classpath. Publish the corresponding core declaration through the same symbol
            // provider as the other language builtins, so checked IR never needs an emitter-only
            // classifier exception merely to format that already-recorded type.
            classifier.type_parameters =
                TypeParameters::invariant(vec!["E".to_string()], vec![Vec::new()]);
        }
        if matches!(name, "EqualityBound" | "Suppress") {
            classifier.kind = TypeKind::Annotation;
        }
        if name == "EqualityBound" {
            classifier.named_parameter_lists.push(ParamList {
                visibility: Visibility::Public,
                names: vec!["bound".to_string()],
                defaults: vec![false],
                types: vec![Ty::obj_args(
                    "kotlin/reflect/KClass",
                    &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
                )],
                recv_fun: vec![false],
                vararg: None,
                annotation: None,
            });
            classifier.retention = Some("SOURCE".to_string());
        }
        if name == "Suppress" {
            // `kotlin.Suppress` is a language builtin: parsing and visibility diagnostics must work
            // even for a standalone frontend with no stdlib artifact. Publish its real Kotlin
            // declaration shape at the provider boundary instead of teaching annotation checking a
            // name-based exception.
            classifier.named_parameter_lists.push(ParamList {
                visibility: Visibility::Public,
                names: vec!["names".to_string()],
                defaults: vec![false],
                types: vec![Ty::obj_args("kotlin/Array", &[Ty::String])],
                recv_fun: vec![false],
                vararg: Some(0),
                annotation: None,
            });
            classifier.retention = Some("SOURCE".to_string());
        }
        if name == "Unit" {
            classifier.kind = TypeKind::Object;
        }
        if name != "Any" && name != "Nothing" {
            classifier.supertypes.push_name(crate::types::wk::any());
            classifier
                .supertype_templates
                .push(Ty::obj_name(crate::types::wk::any()));
        }
        if name == "Nothing" {
            // `Nothing` has no instances, but its expressions have every non-null type as a
            // supertype; `Any` is the only declaration owner needed for member lookup here.
            classifier.supertypes.push_name(crate::types::wk::any());
            classifier
                .supertype_templates
                .push(Ty::obj_name(crate::types::wk::any()));
        }
        if name == "Any" {
            classifier.constructors.push(LibraryMember::new(
                "<init>".to_string(),
                Vec::new(),
                Ty::Unit,
                "()V".to_string(),
            ));
            let declarations = [
                (
                    "equals",
                    vec![Ty::nullable(Ty::obj_name(internal))],
                    Ty::Boolean,
                ),
                ("hashCode", Vec::new(), Ty::Int),
                ("toString", Vec::new(), Ty::String),
            ];
            for (name, params, ret) in declarations {
                let mut callable = LibraryCallable::library(internal, name, params, ret, ret, "");
                if name == "toString" {
                    // The target-free core provider has no dependency registry from which it could
                    // allocate an `ExternalCallableId`. Preserve the selected declaration as the
                    // backend-neutral Kotlin string-conversion operation instead. Target providers
                    // that publish a concrete `Any.toString` declaration keep ordinary dispatch.
                    callable.member_realization =
                        MemberRealization::Intrinsic(CompilerIntrinsic::NullableAnyToString);
                }
                classifier.insert_declared_callables(
                    name.to_string(),
                    Callables::Functions(FunctionSet {
                        overloads: vec![FunctionInfo::plain(FnKind::Member, None, callable)],
                    }),
                );
            }
        }
        add_core_builtin_declarations(&mut classifier, internal);
        Some(classifier)
    }

    fn builtin_kotlin_callables(name: &str) -> Callables {
        let mut functions = FunctionSet::default();
        let mut properties = PropertySet::default();
        match name {
            "plus" => {
                let receiver = Ty::nullable(Ty::String);
                let mut callable = LibraryCallable::library(
                    crate::types::type_name("kotlin"),
                    "plus",
                    vec![receiver, Ty::nullable(Ty::obj("kotlin/Any"))],
                    Ty::String,
                    Ty::String,
                    "",
                );
                callable.compiler_intrinsic = Some(CompilerIntrinsic::StringPlus);
                let mut function = FunctionInfo::plain(FnKind::Extension, Some(receiver), callable);
                function.call_sig = CallSig::metadata_plain(1);
                function.flags.operator = true;
                functions.overloads.push(function);
            }
            "toString" => {
                let receiver = Ty::nullable(Ty::obj("kotlin/Any"));
                let mut callable = LibraryCallable::library(
                    crate::types::type_name("kotlin"),
                    "toString",
                    vec![receiver],
                    Ty::String,
                    Ty::String,
                    "",
                );
                callable.compiler_intrinsic = Some(CompilerIntrinsic::NullableAnyToString);
                let mut function = FunctionInfo::plain(FnKind::Extension, Some(receiver), callable);
                function.call_sig = CallSig::metadata_plain(0);
                functions.overloads.push(function);
            }
            "code" => {
                let owner = crate::types::type_name("kotlin/CharKt");
                let mut getter = LibraryCallable::library(
                    owner,
                    "getCode",
                    vec![Ty::Char],
                    Ty::Int,
                    Ty::Int,
                    "",
                );
                getter.compiler_intrinsic = Some(CompilerIntrinsic::CharCode);
                properties.overloads.push(PropertyInfo {
                    name: name.to_string(),
                    kind: PropKind::Extension,
                    receiver: Some(Ty::Char),
                    formals: Vec::new(),
                    ty: Ty::Int,
                    context_count: 0,
                    context_param_names: Vec::new(),
                    getter,
                    setter: None,
                    setter_visibility: Visibility::Private,
                    is_const: false,
                    implicit_integer_coercion: false,
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
                    read_stability: PropertyReadStability::Unstable,
                });
            }
            _ => {}
        }
        Callables::from_parts(functions, properties)
    }

    fn builtin_text_callables(name: &str) -> Callables {
        let signatures: &[(&[Ty], Ty)] = match name {
            "substring" => &[(&[Ty::Int], Ty::String), (&[Ty::Int, Ty::Int], Ty::String)],
            "indexOf" => &[(&[Ty::String], Ty::Int)],
            "trimIndent" | "trimMargin" => &[(&[], Ty::String)],
            _ => return Callables::None,
        };
        let receiver = Ty::String;
        let owner = crate::types::type_name("kotlin/text/StringsKt");
        let overloads = signatures
            .iter()
            .map(|(params, ret)| {
                let mut callable = LibraryCallable::library(
                    owner,
                    name,
                    std::iter::once(receiver)
                        .chain(params.iter().copied())
                        .collect(),
                    *ret,
                    *ret,
                    "",
                );
                callable.compiler_intrinsic = match name {
                    "trimIndent" => Some(CompilerIntrinsic::TrimIndent),
                    "trimMargin" => Some(CompilerIntrinsic::TrimMargin),
                    _ => None,
                };
                FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
            })
            .collect();
        Callables::Functions(FunctionSet { overloads })
    }

    fn builtin_console_callables(name: &str) -> Callables {
        let arities: &[usize] = match name {
            "print" => &[1],
            "println" => &[0, 1],
            _ => return Callables::None,
        };
        let owner = crate::types::type_name("kotlin/io/ConsoleKt");
        let intrinsic = if name == "print" {
            CompilerIntrinsic::Print
        } else {
            CompilerIntrinsic::Println
        };
        let message = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let overloads = arities
            .iter()
            .map(|arity| {
                let params = if *arity == 0 {
                    Vec::new()
                } else {
                    vec![message]
                };
                let mut callable =
                    LibraryCallable::library(owner, name, params, Ty::Unit, Ty::Unit, "");
                callable.compiler_intrinsic = Some(intrinsic);
                FunctionInfo::plain(FnKind::TopLevel, None, callable)
            })
            .collect();
        Callables::Functions(FunctionSet { overloads })
    }

    fn builtin_coroutine_intrinsic_callables(name: &str) -> Callables {
        if name != "COROUTINE_SUSPENDED" {
            return Callables::None;
        }
        let ty = Ty::obj_name(crate::types::wk::any());
        let mut getter = LibraryCallable::library(
            crate::types::type_name("kotlin/coroutines/intrinsics/IntrinsicsKt"),
            "getCOROUTINE_SUSPENDED",
            Vec::new(),
            ty,
            ty,
            "()Ljava/lang/Object;",
        );
        getter.compiler_intrinsic = Some(CompilerIntrinsic::CoroutineSuspended);
        Callables::Properties(PropertySet {
            overloads: vec![PropertyInfo {
                name: name.to_string(),
                kind: PropKind::TopLevel,
                receiver: None,
                formals: Vec::new(),
                ty,
                context_count: 0,
                context_param_names: Vec::new(),
                getter,
                setter: None,
                setter_visibility: Visibility::Private,
                is_const: false,
                implicit_integer_coercion: false,
                compile_time_constant: None,
                visibility: Visibility::Public,
                owner: crate::types::type_name("kotlin/coroutines/intrinsics/IntrinsicsKt"),
                receiver_rank: 0,
                source_key: None,
                stable_declaration: None,
                getter_declaration: None,
                setter_declaration: None,
                source_member: None,
                accessor_derived: false,
                read_stability: PropertyReadStability::Unstable,
            }],
        })
    }
}

impl crate::symbol_source::SymbolSource for EmptySymbolSource {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        (parent == TypeName::ROOT && name == "kotlin")
            || (parent.matches("kotlin") && matches!(name, "coroutines" | "io" | "text"))
            || (parent.matches("kotlin/coroutines") && name == "intrinsics")
    }

    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<ResolvedSymbols> {
        let crate::symbol_source::SymbolNamespace::Package(package) = namespace else {
            return std::rc::Rc::new(ResolvedSymbols::default());
        };
        if package.matches("kotlin/text") {
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Self::builtin_text_callables(name),
                ..ResolvedSymbols::default()
            });
        }
        if package.matches("kotlin/io") {
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Self::builtin_console_callables(name),
                ..ResolvedSymbols::default()
            });
        }
        if package.matches("kotlin/coroutines/intrinsics") {
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Self::builtin_coroutine_intrinsic_callables(name),
                ..ResolvedSymbols::default()
            });
        }
        if package.matches("kotlin/reflect") {
            // The property reference a delegated property's accessors pass to its convention. The
            // LANGUAGE defines that convention — a `by` clause is refused without it — so the
            // declaration has to be answerable with no stdlib artifact on the target, for the same
            // reason `Enum` is for an enum class's implicit supertype. Only the identity and arity
            // are published here; everything else about it is the real artifact's.
            let Some(internal) = matches!(name, "KProperty")
                .then(|| crate::types::type_name(&format!("kotlin/reflect/{name}")))
            else {
                return std::rc::Rc::new(ResolvedSymbols::default());
            };
            let mut classifier = LibraryType::declaration_header();
            classifier.kind = TypeKind::Interface;
            classifier.type_parameters =
                TypeParameters::invariant(vec!["R".to_string()], vec![Vec::new()]);
            return std::rc::Rc::new(ResolvedSymbols {
                classifier_name: Some(internal),
                classifier: Some(std::sync::Arc::new(classifier)),
                ..ResolvedSymbols::default()
            });
        }
        if !package.matches("kotlin") {
            return std::rc::Rc::new(ResolvedSymbols::default());
        }
        let callables = Self::builtin_kotlin_callables(name);
        let internal = namespace.existing_classifier(name).or_else(|| {
            // These annotations are language builtins even when the target has no stdlib artifact.
            // Unlike scalar `Ty` spellings they have no earlier type construction that interns the
            // identity, so the provider must publish the known declaration identity itself.
            matches!(name, "EqualityBound" | "Suppress")
                .then(|| crate::types::type_name(&format!("kotlin/{name}")))
        });
        let Some(internal) = internal else {
            return std::rc::Rc::new(ResolvedSymbols {
                callables,
                ..ResolvedSymbols::default()
            });
        };
        let Some(classifier) = Self::builtin_classifier(name, internal) else {
            return std::rc::Rc::new(ResolvedSymbols {
                callables,
                ..ResolvedSymbols::default()
            });
        };
        std::rc::Rc::new(ResolvedSymbols {
            classifier_name: Some(internal),
            classifier: Some(std::sync::Arc::new(classifier)),
            callables,
            importable_declaration: false,
        })
    }
}
