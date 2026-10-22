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
        let formal = signature.formals[0].clone();
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
