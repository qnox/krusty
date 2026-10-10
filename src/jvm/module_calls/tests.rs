use super::{realize_super_calls, ModuleRealizationTarget};
use crate::backend::CheckedBackendCallables;
use crate::fir::{ExternalCallableId, ResolvedFunctionOverrideTarget};
use crate::ir::{Callee, IrExpr, IrFile, IrSuperCallKind};
use crate::jvm::property_realizations::PropertyRealizations;
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, LibraryCallable, MemberRealization,
    NonvirtualCallRealization,
};
use crate::types::{type_name, Ty};

fn dependency_super_call(ir: &mut IrFile, target: ExternalCallableId) -> crate::ir::ExprId {
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.add_expr(IrExpr::Call {
        callee: Callee::Super {
            owner: type_name("fixture/Legacy"),
            dispatch_owner: type_name("fixture/Child"),
            enclosing_dispatch: false,
            kind: IrSuperCallKind::Function,
            name: "value".to_string(),
            params: Vec::new(),
            ret: Ty::Int,
            interface: true,
            realization: MemberRealization::Dispatch,
            descriptor: "()I".to_string(),
            declaration: Some(ResolvedFunctionOverrideTarget::External(target)),
            defaults: Vec::new(),
            source_member: None,
        },
        dispatch_receiver: Some(receiver),
        args: Vec::new(),
    })
}

#[test]
fn a_dependency_super_call_requires_its_frozen_identity() {
    let mut ir = IrFile::default();
    let target = ExternalCallableId::from_raw(4);
    dependency_super_call(&mut ir, target);

    assert_eq!(
        realize_super_calls(
            &mut ir,
            &CheckedBackendCallables::default(),
            &mut PropertyRealizations::default(),
            crate::jvm::ir_emit::JvmDefaultMode::default(),
        ),
        Err(ModuleRealizationTarget::DependencyCallable(target))
    );
}

#[test]
fn a_dependency_super_call_uses_its_frozen_nonvirtual_target() {
    let mut ir = IrFile::default();
    let target = ExternalCallableId::from_raw(4);
    let call = dependency_super_call(&mut ir, target);
    let holder = NonvirtualCallRealization {
        owner: type_name("fixture/Legacy$DefaultImpls"),
        descriptor: "(Lfixture/Legacy;)I".to_string(),
    };
    let mut declaration = LibraryCallable::library(
        type_name("fixture/Legacy"),
        "value",
        Vec::new(),
        Ty::Int,
        Ty::Int,
        "()I",
    );
    declaration.owner_is_interface = true;
    declaration.nonvirtual_realization = Some(Box::new(holder.clone()));
    let realization = ExternalCallableRealization {
        callable: declaration,
        kind: ExternalCallableKind::Member,
        declaration_owner: None,
        parameter_identities: Box::new([]),
        declaration_signature: None,
    };
    let mut facts = CheckedBackendCallables::default();
    facts
        .freeze_plugin_super_callables(&ir, |identity| {
            (identity == target).then(|| realization.clone())
        })
        .expect("the selected super declaration has a frozen fact");

    realize_super_calls(
        &mut ir,
        &facts,
        &mut PropertyRealizations::default(),
        crate::jvm::ir_emit::JvmDefaultMode::default(),
    )
    .expect("the frozen holder realizes the super call");
    let IrExpr::Call {
        callee:
            Callee::Static {
                owner,
                name,
                descriptor,
                ..
            },
        dispatch_receiver: Some(_),
        args,
    } = ir.expr(call)
    else {
        panic!("the legacy super call was not realized as its holder static");
    };
    assert_eq!(*owner, holder.owner);
    assert_eq!(name, "value");
    assert_eq!(descriptor, &holder.descriptor);
    assert!(args.is_empty());
}
