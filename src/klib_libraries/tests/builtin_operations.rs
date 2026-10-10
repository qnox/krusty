//! Builtin declarations of the Kotlin/Native stdlib KLIB carry the shared compiler operations.

use super::super::classifier_records::classifier_record;
use super::super::lookup::declared_symbols;
use super::analysis::native_stdlib;
use crate::libraries::{CompilerIntrinsic, MemberRealization, PrimitiveBinaryIntrinsic};
use crate::symbol_source::SymbolNamespace;
use crate::types::{type_name, ArrayFactoryKind, SemanticCallRole, Ty};

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
