use super::*;
use crate::ir::IrFunction;

fn function(ir: &mut IrFile, params: Vec<Ty>, ret: Ty) -> FunId {
    ir.add_fun(IrFunction {
        name: "sample".to_owned(),
        param_checks: vec![None; params.len()],
        params,
        ret,
        body: None,
        is_static: true,
        dispatch_receiver: None,
    })
}

#[test]
fn the_emitted_signature_keeps_formals_even_with_scalar_upper_bounds() {
    let mut ir = IrFile::default();
    let formal = Ty::ty_param("declaration-owned-T", Ty::Int);
    let id = function(&mut ir, vec![formal], formal);
    let signature = DependencyFunctionSignature::from_ir(&ir, id, None).unwrap();
    assert_eq!(signature.parameters(), [formal]);
    assert_eq!(signature.result(), formal);
}

#[test]
fn the_emitted_signature_authenticates_retained_member_dispatch() {
    let mut ir = IrFile::default();
    let id = function(&mut ir, vec![Ty::String], Ty::Long);
    let owner = crate::types::type_name("sample/Owner");
    ir.functions[id as usize].dispatch_receiver = Some(owner);
    let mut exports = super::super::DependencyExportSignatures::default();
    exports.record_member_receiver(id, Ty::obj_name(owner));
    let signature =
        DependencyFunctionSignature::from_ir(&ir, id, exports.member_receiver(id)).unwrap();
    assert_eq!(signature.parameters(), [Ty::obj_name(owner), Ty::String]);
    assert_eq!(signature.result(), Ty::Long);
    assert_eq!(
        DependencyFunctionSignature::from_ir(&ir, id, None),
        Err("a dependency export whose member receiver disagrees with its body")
    );
}

fn generic() -> GenericSig {
    let formal = Ty::ty_param("dependency-owned-T", Ty::nullable(Ty::obj("kotlin/Any")));
    GenericSig {
        formals: vec![crate::types::TyParamName::new("dependency-owned-T")],
        formal_bounds: vec![Vec::new()],
        receiver: Some(formal),
        params: vec![Ty::Long, formal],
        ret: formal,
        return_policy: Default::default(),
    }
}

fn argument(callable: u32, ordinal: u32, value: Ty) -> IrCheckedSubstitution {
    IrCheckedSubstitution {
        parameter: FirTypeParameterRef::External {
            callable: ExternalCallableId::from_raw(callable),
            ordinal,
        },
        reified: false,
        value,
        reified_runtime: value,
        additional_bounds: Vec::new(),
    }
}

fn checked_call(
    parameters: Vec<Ty>,
    result: Ty,
    substitutions: Vec<IrCheckedSubstitution>,
) -> Callee {
    Callee::External {
        target: ExternalCallableId::from_raw(7),
        default_provider: None,
        params: parameters,
        ret: result,
        substitutions,
        defaults: Vec::new(),
        extension_receiver_parameter: None,
    }
}

fn frozen(signature: GenericSig) -> BackendCallableFact {
    BackendCallableFact {
        name: "sample".to_owned(),
        physical_name: None,
        reflection_name: None,
        physical_owner: crate::types::TypeName::ROOT,
        visibility: crate::types::Visibility::Public,
        declaration_owner: None,
        singleton_dispatch: None,
        kind: crate::libraries::ExternalCallableKind::Extension,
        owner_is_interface: false,
        compiler_intrinsic: None,
        semantic_role: None,
        overridden_declarations: Box::new([]),
        member_realization: crate::libraries::MemberRealization::Dispatch,
        params: signature.parameters_with_receiver(1).into_vec(),
        physical_params: Vec::new(),
        parameter_identities: Box::new([]),
        physical_parameter_plan: None,
        ret: signature.ret,
        physical_ret: signature.ret,
        descriptor: String::new(),
        inline: crate::libraries::InlineKind::None,
        source_receiver: signature.receiver,
        context_count: 1,
        declared_params: None,
        declared_ret: None,
        inline_modifiers: Box::new([]),
        default_realization: None,
        nonvirtual_realization: None,
        overridden_call_realizations: Box::new([]),
        generic_sig: Some(Box::new(signature)),
        reified_type_parameter_ordinals: Box::new([]),
        declaration_signature: None,
        dependency_callable: None,
        direct_c_function: None,
        klib_body_companion_receiver: None,
        klib_body_type_parameters: Box::new([]),
        klib_body_declaration_modifiers: None,
        klib_body_class_contract: None,
        klib_body_accessor: None,
        klib_body_vararg_index: None,
        klib_body_parameter_defaults: None,
        annotations: Box::new([]),
    }
}

#[test]
fn a_frozen_generic_declaration_is_instantiated_without_changing_its_contract() {
    let mut fact = frozen(generic());
    for actual in [Ty::Int, Ty::String, Ty::Long] {
        let instantiated = instantiate_dependency_signature(
            &fact,
            &checked_call(
                vec![Ty::Long, actual, actual],
                actual,
                vec![argument(7, 0, actual)],
            ),
        )
        .unwrap();
        assert_eq!(&*instantiated.parameters, [Ty::Long, actual, actual]);
        assert_eq!(instantiated.receiver, Some(actual));
        assert_eq!(instantiated.result, actual);
    }
    assert_eq!(fact.generic_sig.as_deref(), Some(&generic()));
    fact.params[0] = Ty::Int;
    assert_eq!(
        instantiate_dependency_signature(
            &fact,
            &checked_call(
                vec![Ty::Long, Ty::Int, Ty::Int],
                Ty::Int,
                vec![argument(7, 0, Ty::Int)]
            ),
        )
        .err(),
        Some("a dependency generic signature disagrees with its frozen declaration")
    );
}

mod class_application;

#[test]
fn checked_instantiation_keeps_the_context_boundary_for_every_carrier() {
    let signature = generic();
    for actual in [Ty::Int, Ty::String, Ty::Long] {
        let instantiated = instantiate(
            &signature,
            1,
            ExternalCallableId::from_raw(7),
            &[argument(7, 0, actual)],
        )
        .unwrap();
        assert_eq!(&*instantiated.parameters, [Ty::Long, actual, actual]);
        assert_eq!(instantiated.receiver, Some(actual));
        assert_eq!(instantiated.result, actual);
    }
    // Instantiation never mutates the declaration which owns the canonical generic boundary.
    assert_eq!(signature, generic());
}

#[test]
fn only_complete_exact_ordinary_checked_substitutions_instantiate_an_export() {
    let mut reified = argument(7, 0, Ty::Int);
    reified.reified = true;
    let cases = [
        (vec![], "a dependency call is missing a type substitution"),
        (
            vec![argument(8, 0, Ty::Int)],
            "a dependency call carries a foreign type substitution",
        ),
        (
            vec![argument(7, 1, Ty::Int)],
            "a dependency call has an out-of-range type substitution",
        ),
        (
            vec![argument(7, 0, Ty::Int), argument(7, 0, Ty::String)],
            "a dependency call repeats a type substitution",
        ),
        (
            vec![reified],
            "a reified dependency cannot use an ordinary generic export",
        ),
    ];
    for (arguments, expected) in cases {
        assert_eq!(
            instantiate(&generic(), 1, ExternalCallableId::from_raw(7), &arguments).err(),
            Some(expected)
        );
    }
}
