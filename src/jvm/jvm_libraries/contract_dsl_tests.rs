//! The contract DSL is recognized by the complete signature of the declaration a call selected.

use super::JvmLibraries;
use crate::contracts::{DslMember, SelectedDslCallable};
use crate::libraries::{FunctionInfo, SemanticPlatform};
use crate::symbol_source::SymbolNamespace;
use crate::types::{type_name, Ty};

fn stdlib_libraries() -> Option<JvmLibraries> {
    let stdlib = crate::toolchain::stdlib_jar()?;
    Some(
        JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("JVM provider initialization"),
    )
}

fn members(libraries: &JvmLibraries, owner: &str, name: &str) -> Vec<FunctionInfo> {
    crate::symbol_resolver::declared_member_callables(libraries, Ty::obj(owner), name)
        .functions()
        .to_vec()
}

fn selected(function: &FunctionInfo) -> SelectedDslCallable<'_> {
    SelectedDslCallable {
        owner: function.callable.owner,
        name: &function.callable.name,
        dispatch_member: function.kind == crate::libraries::FnKind::Member,
        context_parameters: function.context_count,
        params: &function.callable.params[function.context_count..],
        ret: function.callable.ret,
    }
}

#[test]
fn the_declared_dsl_members_are_recognized() {
    let Some(libraries) = stdlib_libraries() else {
        return;
    };
    let mut recognized = Vec::new();
    for (owner, name) in [
        ("kotlin/contracts/ContractBuilder", "returns"),
        ("kotlin/contracts/ContractBuilder", "returnsNotNull"),
        ("kotlin/contracts/ContractBuilder", "callsInPlace"),
        ("kotlin/contracts/SimpleEffect", "implies"),
    ] {
        for function in members(&libraries, owner, name) {
            recognized.push(libraries.contract_dsl_member(&selected(&function)));
        }
    }
    assert_eq!(
        recognized,
        [
            Some(DslMember::Returns),
            Some(DslMember::ReturnsValue),
            Some(DslMember::ReturnsNotNull),
            Some(DslMember::CallsInPlace),
            Some(DslMember::Implies),
        ]
    );
}

#[test]
fn a_same_owner_same_name_same_arity_member_of_another_signature_is_not_the_dsl() {
    let Some(libraries) = stdlib_libraries() else {
        return;
    };
    let builder = "kotlin/contracts/ContractBuilder";
    let [_, returns_value] = members(&libraries, builder, "returns")
        .try_into()
        .unwrap_or_else(|_| panic!("ContractBuilder declares returns() and returns(value)"));
    let [calls_in_place] = members(&libraries, builder, "callsInPlace")
        .try_into()
        .unwrap_or_else(|_| panic!("ContractBuilder declares one callsInPlace"));
    let [implies] = members(&libraries, "kotlin/contracts/SimpleEffect", "implies")
        .try_into()
        .unwrap_or_else(|_| panic!("SimpleEffect declares one implies"));
    let foreign_kind = Ty::obj("p/InvocationKind");
    let variants = [
        // `returns(value: Int)`, and `returns(value: Any?)` returning another type.
        SelectedDslCallable {
            params: &[Ty::Int],
            ..selected(&returns_value)
        },
        SelectedDslCallable {
            ret: Ty::obj("kotlin/contracts/ReturnsNotNull"),
            ..selected(&returns_value)
        },
        // `callsInPlace(lambda: Function<R>, kind: p.InvocationKind)`, and one taking a plain
        // function value instead of `Function<R>`.
        SelectedDslCallable {
            params: &[calls_in_place.callable.params[0], foreign_kind],
            ..selected(&calls_in_place)
        },
        SelectedDslCallable {
            params: &[Ty::obj("kotlin/Any"), calls_in_place.callable.params[1]],
            ..selected(&calls_in_place)
        },
        // `implies(booleanExpression: Int)`, and an extension rather than the member.
        SelectedDslCallable {
            params: &[Ty::Int],
            ..selected(&implies)
        },
        SelectedDslCallable {
            dispatch_member: false,
            ..selected(&implies)
        },
    ];
    for variant in variants {
        assert_eq!(libraries.contract_dsl_member(&variant), None, "{variant:?}");
    }
}

#[test]
fn only_the_intrinsic_taking_a_contract_builder_lambda_is_erased() {
    let Some(libraries) = stdlib_libraries() else {
        return;
    };
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/contracts")),
        "contract",
    );
    let [contract] = symbols.callables.functions() else {
        panic!("kotlin.contracts declares exactly one contract");
    };
    assert!(libraries.is_erased_contract_callable(&contract.callable));
    let receiver_fun = |params| Ty::fun_with_shape(params, Ty::Unit, 0, true, false);
    let other_receiver = receiver_fun(vec![Ty::obj("kotlin/Any")]);
    let extra_parameter = receiver_fun(vec![Ty::obj("kotlin/contracts/ContractBuilder"), Ty::Int]);
    let no_receiver = Ty::fun(vec![Ty::obj("kotlin/contracts/ContractBuilder")], Ty::Unit);
    let suspend = Ty::fun_with_shape(
        vec![Ty::obj("kotlin/contracts/ContractBuilder")],
        Ty::Unit,
        0,
        true,
        true,
    );
    for params in [
        vec![other_receiver],
        vec![extra_parameter],
        vec![no_receiver],
        vec![suspend],
    ] {
        let mut variant = contract.callable.clone();
        variant.params = params;
        assert!(
            !libraries.is_erased_contract_callable(&variant),
            "{:?}",
            variant.params
        );
    }
    let mut returns_int = contract.callable.clone();
    returns_int.ret = Ty::Int;
    assert!(!libraries.is_erased_contract_callable(&returns_int));
}
