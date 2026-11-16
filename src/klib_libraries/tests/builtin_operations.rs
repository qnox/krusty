//! Builtin declarations of the Kotlin/Native stdlib KLIB carry the shared compiler operations.

use super::super::classifier_records::classifier_record;
use super::super::lookup::declared_symbols;
use super::analysis::{native_stdlib, target_stdlib};
use crate::libraries::{
    CompilerIntrinsic, MemberRealization, PrimitiveBinaryIntrinsic, ResultFailureIntrinsic,
    StdlibKlibIntrinsic,
};
use crate::symbol_source::SymbolNamespace;
use crate::types::{type_name, ArrayFactoryKind, SemanticCallRole, Ty};

#[test]
fn language_function_invocations_have_opaque_classifier_owned_identities() {
    use super::super::external_identities::ExternalIdentities;
    use super::super::inventory::PackageInventory;

    let inventory = PackageInventory::from_packages(Vec::new()).expect("empty KLIB inventory");
    let identities = ExternalIdentities::default();
    for owner in [
        type_name("kotlin/Function1"),
        type_name("kotlin/coroutines/SuspendFunction1"),
    ] {
        let symbols = declared_symbols(
            &inventory,
            &identities,
            SymbolNamespace::Package(owner.parent().expect("function classifier package")),
            owner.segment_ref(),
        );
        let classifier = symbols.classifier.expect("language classifier record");
        let invocations = classifier
            .declared_callables
            .get("invoke")
            .expect("language invoke declaration")
            .functions();
        let [invoke] = invocations else {
            panic!("{owner} declares one language invoke candidate")
        };
        assert_eq!(
            invoke.callable.semantic_role,
            Some(SemanticCallRole::KotlinFunctionInvoke)
        );
        assert_eq!(
            invoke.callable.declaration_owner,
            Some(crate::types::SemanticCallableOwner::Classifier(owner))
        );
        let identity = invoke
            .callable
            .external_identity
            .expect("opaque invocation identity");
        let realization = identities
            .realization(identity)
            .expect("published realization");
        assert_eq!(
            realization.callable.semantic_role,
            Some(SemanticCallRole::KotlinFunctionInvoke)
        );
        assert_eq!(
            realization.callable.declaration_owner,
            Some(crate::types::SemanticCallableOwner::Classifier(owner))
        );
        assert_eq!(
            realization.kind,
            crate::libraries::ExternalCallableKind::Member
        );
        assert!(realization.declaration_signature.is_none());
        assert!(realization.dependency_callable.is_none());
        assert_eq!(
            realization.callable.suspend,
            owner.matches("kotlin/coroutines/SuspendFunction1")
        );
        let repeated = declared_symbols(
            &inventory,
            &identities,
            SymbolNamespace::Package(owner.parent().expect("function classifier package")),
            owner.segment_ref(),
        );
        let repeated = repeated
            .classifier
            .expect("repeated language classifier record")
            .declared_callables["invoke"]
            .functions()[0]
            .callable
            .external_identity;
        assert_eq!(repeated, Some(identity), "the provider identity is stable");
        let static_members = declared_symbols(
            &inventory,
            &identities,
            SymbolNamespace::Classifier(owner),
            "invoke",
        );
        assert!(
            static_members.callables.functions().is_empty(),
            "an instance member is not published as a classifier-associated callable"
        );
    }
}

#[test]
fn klib_string_plus_requires_selected_stdlib_authority() {
    use crate::symbol_source::SymbolSource;
    let Some(trusted) = native_stdlib() else {
        return;
    };
    let path =
        crate::toolchain::kotlin_stdlib_klib(crate::compilation_target::CompilationTarget::Native)
            .unwrap();
    let untrusted =
        super::super::KlibLibraries::open(&[super::super::KlibLibraryInput::dependency(path)])
            .unwrap();
    for (provider, trusted) in [(&trusted, true), (&untrusted, false)] {
        let class = classifier_record(
            &provider.inventory,
            &provider.identities,
            type_name("kotlin/String"),
        )
        .unwrap();
        let package = declared_symbols(
            &provider.inventory,
            &provider.identities,
            SymbolNamespace::Package(type_name("kotlin")),
            "plus",
        );
        let member = &class.declared_callables["plus"];
        let mut operations = Vec::new();
        for function in member
            .functions()
            .iter()
            .chain(package.callables.functions())
            .filter(|function| {
                function.callable.name == "plus"
                    && function.semantic_params().as_ref() == [Ty::nullable(Ty::obj("kotlin/Any"))]
                    && (function.kind == crate::libraries::FnKind::Member
                        || function.semantic_receiver() == Some(Ty::nullable(Ty::String)))
            })
        {
            let selected = provider
                .external_callable(function.callable.external_identity.unwrap())
                .unwrap();
            let identity = selected.dependency_callable.as_ref().unwrap();
            let (_, serialized) = provider.external_callable_by_dependency(identity).unwrap();
            operations.push([
                function
                    .callable
                    .compiler_intrinsic
                    .or(function.callable.member_realization.compiler_intrinsic()),
                selected.klib_body_callable().unwrap().compiler_intrinsic(),
                serialized
                    .klib_body_callable()
                    .unwrap()
                    .compiler_intrinsic(),
            ]);
        }
        assert_eq!(
            operations,
            [[trusted.then_some(CompilerIntrinsic::StringPlus); 3]; 2],
            "selected stdlib: {trusted}"
        );
    }
}

#[test]
fn only_the_selected_native_stdlib_realizes_its_result_check_helpers() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let package = type_name("kotlin/test");
    let name = "checkResultIsFailure";
    // The exact same archive bytes, including their manifest module identity, cannot grant
    // compiler operations when compilation input assembly selects them as an ordinary dependency.
    let path =
        crate::toolchain::kotlin_stdlib_klib(crate::compilation_target::CompilationTarget::Native)
            .expect("native_stdlib already required this archive");
    let untrusted =
        super::super::KlibLibraries::open(&[super::super::KlibLibraryInput::dependency(path)])
            .expect("the identical ordinary dependency declarations are signable");
    for (provider, trusted) in [(&libraries, true), (&untrusted, false)] {
        let symbols = declared_symbols(
            &provider.inventory,
            &provider.identities,
            SymbolNamespace::Package(package),
            name,
        );
        assert_eq!(symbols.callables.functions().len(), 2);
        for function in symbols.callables.functions() {
            let expected = if function.semantic_params().len() == 3 {
                ResultFailureIntrinsic::CheckOfClass
            } else {
                ResultFailureIntrinsic::Check
            };
            assert_eq!(
                function.callable.compiler_intrinsic,
                trusted.then_some(CompilerIntrinsic::ResultFailure(expected)),
                "trusted={trusted}: {:?}",
                function.semantic_params()
            );
        }
    }
}

#[test]
fn an_int_plus_member_is_the_primitive_addition() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let record = classifier_record(
        &libraries.inventory,
        &libraries.identities,
        type_name("kotlin/Int"),
    )
    .expect("the stdlib declares kotlin.Int");
    let realizations: Vec<_> = record.declared_callables["plus"]
        .functions()
        .iter()
        .filter(|function| function.callable.params == [Ty::Int])
        .map(|function| function.callable.member_realization)
        .collect();
    assert_eq!(
        realizations,
        [MemberRealization::Intrinsic(
            CompilerIntrinsic::PrimitiveBinary(PrimitiveBinaryIntrinsic::Add)
        )]
    );
}

#[test]
fn an_int_array_of_is_the_primitive_array_factory() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let symbols = declared_symbols(
        &libraries.inventory,
        &libraries.identities,
        SymbolNamespace::Package(type_name("kotlin")),
        "intArrayOf",
    );
    let intrinsics: Vec<_> = symbols
        .callables
        .functions()
        .iter()
        .map(|function| function.callable.compiler_intrinsic)
        .collect();
    assert_eq!(
        intrinsics,
        [Some(CompilerIntrinsic::ArrayFactory(
            ArrayFactoryKind::PrimitiveVararg(Ty::Int)
        ))]
    );
}

#[test]
fn a_kcallable_name_property_is_the_callable_reference_name() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let record = classifier_record(
        &libraries.inventory,
        &libraries.identities,
        type_name("kotlin/reflect/KCallable"),
    )
    .expect("the stdlib declares kotlin.reflect.KCallable");
    let roles: Vec<_> = record.declared_callables["name"]
        .properties()
        .iter()
        .map(|property| property.getter.semantic_role)
        .collect();
    assert_eq!(roles, [Some(SemanticCallRole::KotlinCallableReferenceName)]);
}

#[test]
fn target_stdlib_list_factories_publish_their_exact_storage_plans() {
    use crate::libraries::ListStorageConstruction;
    for target in [
        crate::compilation_target::CompilationTarget::Native,
        crate::compilation_target::CompilationTarget::WasmJs,
        crate::compilation_target::CompilationTarget::WasmWasi,
    ] {
        let Some(libraries) = target_stdlib(target) else {
            continue;
        };
        for (name, expected) in [
            ("emptyList", &[ListStorageConstruction::ReadonlyEmpty][..]),
            (
                "listOf",
                &[
                    ListStorageConstruction::ReadonlyEmpty,
                    ListStorageConstruction::ReadonlySingleton,
                    ListStorageConstruction::ReadonlyPacked,
                ][..],
            ),
            (
                "mutableListOf",
                &[
                    ListStorageConstruction::MutableEmpty,
                    ListStorageConstruction::MutablePacked,
                ][..],
            ),
            (
                "arrayListOf",
                &[
                    ListStorageConstruction::MutableEmpty,
                    ListStorageConstruction::MutablePacked,
                ][..],
            ),
        ] {
            let symbols = declared_symbols(
                &libraries.inventory,
                &libraries.identities,
                SymbolNamespace::Package(type_name("kotlin/collections")),
                name,
            );
            let functions = symbols.callables.functions();
            assert_eq!(
                functions.len(),
                expected.len(),
                "{target:?} {name} overloads"
            );
            for &plan in expected {
                assert_eq!(
                    functions
                        .iter()
                        .filter(|function| function.callable.compiler_intrinsic
                            == Some(CompilerIntrinsic::ListStorageConstruction(plan)))
                        .count(),
                    1,
                    "{target:?} {name} {plan:?}"
                );
            }
        }
    }
}

#[test]
fn target_klibs_attach_unsigned_bitwise_operations_to_the_exact_members() {
    for target in [
        crate::compilation_target::CompilationTarget::Native,
        crate::compilation_target::CompilationTarget::WasmJs,
        crate::compilation_target::CompilationTarget::WasmWasi,
    ] {
        let Some(libraries) = target_stdlib(target) else {
            continue;
        };
        for (owner, element) in [("kotlin/UInt", Ty::UInt), ("kotlin/ULong", Ty::ULong)] {
            let record = classifier_record(
                &libraries.inventory,
                &libraries.identities,
                type_name(owner),
            )
            .unwrap_or_else(|| panic!("{target:?} stdlib declares {owner}"));
            for (name, params, intrinsic) in [
                ("and", vec![element], CompilerIntrinsic::PrimitiveBitAnd),
                ("or", vec![element], CompilerIntrinsic::PrimitiveBitOr),
                ("xor", vec![element], CompilerIntrinsic::PrimitiveBitXor),
                ("inv", Vec::new(), CompilerIntrinsic::PrimitiveBitNot),
                ("shl", vec![Ty::Int], CompilerIntrinsic::PrimitiveShiftLeft),
                (
                    "shr",
                    vec![Ty::Int],
                    CompilerIntrinsic::PrimitiveUnsignedShiftRight,
                ),
            ] {
                let realizations: Vec<_> = record.declared_callables[name]
                    .functions()
                    .iter()
                    .filter(|function| {
                        function.semantic_params().as_ref() == params
                            && function.callable.ret == element
                    })
                    .map(|function| function.callable.member_realization)
                    .collect();
                assert_eq!(
                    realizations,
                    [MemberRealization::Intrinsic(intrinsic)],
                    "{target:?} {owner}.{name}"
                );
            }
        }
    }
}

#[test]
fn target_klibs_authenticate_the_exact_floating_range_builders() {
    for target in [
        crate::compilation_target::CompilationTarget::Native,
        crate::compilation_target::CompilationTarget::WasmJs,
        crate::compilation_target::CompilationTarget::WasmWasi,
    ] {
        let Some(libraries) = target_stdlib(target) else {
            continue;
        };
        let functions = declared_symbols(
            &libraries.inventory,
            &libraries.identities,
            SymbolNamespace::Package(type_name("kotlin/ranges")),
            "rangeTo",
        );
        for scalar in [Ty::Float, Ty::Double] {
            let realizations: Vec<_> = functions
                .callables
                .functions()
                .iter()
                .filter(|function| {
                    function.semantic_receiver() == Some(scalar)
                        && function.semantic_params().as_ref() == [scalar]
                })
                .map(|function| function.callable.compiler_intrinsic)
                .collect();
            assert_eq!(
                realizations,
                [Some(CompilerIntrinsic::FloatingRangeMembership)],
                "{target:?} {scalar:?}.rangeTo({scalar:?})"
            );
        }
    }
}

#[test]
fn target_klibs_authenticate_only_the_complete_enum_entries_declaration() {
    for target in [
        crate::compilation_target::CompilationTarget::Native,
        crate::compilation_target::CompilationTarget::WasmJs,
        crate::compilation_target::CompilationTarget::WasmWasi,
    ] {
        let Some(libraries) = target_stdlib(target) else {
            continue;
        };
        let functions = declared_symbols(
            &libraries.inventory,
            &libraries.identities,
            SymbolNamespace::Package(type_name("kotlin/enums")),
            "enumEntries",
        );
        let authenticated = functions
            .callables
            .functions()
            .iter()
            .filter(|function| {
                function.callable.compiler_intrinsic == Some(CompilerIntrinsic::EnumEntries)
            })
            .collect::<Vec<_>>();
        let [exact] = authenticated.as_slice() else {
            panic!(
                "{target:?} published {} authenticated enumEntries declarations instead of one",
                authenticated.len()
            );
        };
        let exact = *exact;
        let role = StdlibKlibIntrinsic::EnumEntries;
        assert!(role.accepts_normalized_klib(exact));
        assert!(!role.accepts_source(exact));

        let mut wrong_formals = exact.clone();
        wrong_formals.generic_sig.as_mut().unwrap().formals.clear();
        assert!(!role.accepts_normalized_klib(&wrong_formals));

        let mut wrong_bound = exact.clone();
        wrong_bound.generic_sig.as_mut().unwrap().formal_bounds = vec![vec![Ty::obj("kotlin/Any")]];
        assert!(!role.accepts_normalized_klib(&wrong_bound));

        let mut source_shaped_bound = exact.clone();
        let signature = source_shaped_bound.generic_sig.as_mut().unwrap();
        let formal = signature.formals[0];
        let unbounded = Ty::ty_param(&formal, Ty::nullable(Ty::obj("kotlin/Any")));
        signature.formal_bounds = vec![vec![Ty::obj_args("kotlin/Enum", &[unbounded])]];
        assert!(!role.accepts_normalized_klib(&source_shaped_bound));

        let mut wrong_result = exact.clone();
        wrong_result.generic_sig.as_mut().unwrap().ret = Ty::obj("kotlin/enums/EnumEntries");
        assert!(!role.accepts_normalized_klib(&wrong_result));
    }
}

#[test]
fn wasm_klibs_authenticate_the_mixed_closed_range_contains_overloads() {
    for target in [
        crate::compilation_target::CompilationTarget::WasmJs,
        crate::compilation_target::CompilationTarget::WasmWasi,
    ] {
        let Some(libraries) = target_stdlib(target) else {
            continue;
        };
        let functions = declared_symbols(
            &libraries.inventory,
            &libraries.identities,
            SymbolNamespace::Package(type_name("kotlin/ranges")),
            "contains",
        );
        for (receiver, parameter) in [
            (Ty::obj("kotlin/ranges/IntRange"), Ty::Long),
            (Ty::obj("kotlin/ranges/LongRange"), Ty::Int),
            (
                Ty::obj_args("kotlin/ranges/ClosedRange", &[Ty::Float]),
                Ty::Double,
            ),
            (
                Ty::obj_args("kotlin/ranges/ClosedRange", &[Ty::Double]),
                Ty::Float,
            ),
        ] {
            let realizations: Vec<_> = functions
                .callables
                .functions()
                .iter()
                .filter(|function| {
                    function.semantic_receiver() == Some(receiver)
                        && function.semantic_params().as_ref() == [parameter]
                })
                .map(|function| function.callable.compiler_intrinsic)
                .collect();
            assert_eq!(
                realizations,
                [Some(CompilerIntrinsic::ClosedRangeContains)],
                "{target:?} {receiver:?}.contains({parameter:?})"
            );
        }
    }
}
