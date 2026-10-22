//! A constructor's class-owned parameters survive provider and checked-IR handoffs.

use super::{distribution_root, stdlib};
use crate::backend::CheckedBackendCallables;
use crate::ir::{IrConst, IrExpr, IrFile};
use crate::libraries::{ExternalCallableKind, KlibDeclarationSignature};
use crate::metadata::id_signature::{decode_public_id_signature, KlibPublicIdSignature};
use crate::metadata::klib_ir::KlibIrSignature;
use crate::metadata::protobuf::Pb;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, SemanticCallableOwner, Ty, TyKind, TypeVariance};

fn different_mask(signature: &KlibPublicIdSignature) -> KlibPublicIdSignature {
    let mut strings = Vec::new();
    let mut common = Pb::new();
    for (field, segments) in [
        (1, signature.package().segments()),
        (2, signature.declaration().segments()),
    ] {
        for segment in segments {
            common.field_varint(field, strings.len() as u64);
            strings.push(segment.clone());
        }
    }
    if let Some(member) = signature.member_id() {
        common.field_fixed64(6, member);
    }
    common.field_varint(4, signature.mask() ^ 1);
    let mut wrapper = Pb::new();
    wrapper.field_message(1, &common);
    decode_public_id_signature(wrapper.as_bytes(), &strings)
        .expect("a complete public signature")
        .expect("the public signature kind")
}

#[test]
fn klib_constructor_class_contract_survives_declared_and_frozen_views() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let owner = type_name("kotlin/Pair");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "Pair");
    let class = symbols.classifier.as_ref().expect("the stdlib Pair class");
    let [constructor] = class.constructors.as_slice() else {
        panic!("Pair's one primary constructor");
    };
    let target = constructor
        .external_identity
        .expect("the selected constructor");
    let provider = libraries.external_callable(target).expect("provider facts");
    assert_eq!(provider.kind, ExternalCallableKind::Constructor);
    assert_eq!(
        provider.declaration_owner,
        Some(SemanticCallableOwner::Classifier(owner))
    );
    assert_eq!(provider.callable.ret, Ty::Unit);
    assert!(provider.callable.generic_sig.is_none());
    let selected = provider.klib_body_callable().expect("a KLIB declaration");
    let identity = selected.identity().clone();
    assert!(selected.type_parameters().is_empty());
    let contract = selected
        .class_contract()
        .expect("a constructor retains its declaring class contract");
    assert_eq!(contract.module, identity.module);
    assert_eq!(contract.classifier, owner);
    assert_eq!(contract.signature, KlibPublicIdSignature::classifier(owner));
    assert_eq!(contract.parameters.own_count, 2);
    assert_eq!(contract.parameters.formals.len(), 2);
    assert_ne!(
        contract.parameters.formals[0],
        contract.parameters.formals[1]
    );
    assert_eq!(
        contract.parameters.variances.as_ref(),
        [TypeVariance::Out, TypeVariance::Out]
    );
    assert_eq!(
        contract.parameters.bounds.as_ref(),
        class.type_param_bounds().as_slice()
    );
    // Metadata may omit the implicit Any? bound; retain its exact declaration representation.
    for formal in &contract.parameters.formals {
        assert!(matches!(formal.kind(), TyKind::TyParam(_, bound)
            if bound == Ty::nullable(Ty::obj("kotlin/Any"))));
    }
    assert_eq!(
        provider.callable.params,
        contract.parameters.formals.as_ref()
    );
    let raw = libraries
        .declaration_bodies()
        .primitive_member_header(&identity, &contract.signature)
        .expect("the exact declaring class")
        .expect("the constructor is a direct class member");
    assert!(raw.function.constructor);
    assert_eq!(
        raw.class.base.symbol.signature,
        KlibIrSignature::Public(contract.signature.clone())
    );
    let (resolved_target, resolved) = libraries
        .external_callable_by_dependency(&identity)
        .expect("the exact serialized constructor");
    assert_eq!(resolved_target, target);
    assert_eq!(resolved.klib_body_class_contract.as_ref(), Some(contract));

    let mut ir = IrFile::default();
    let first = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let second = ir.add_expr(IrExpr::Const(IrConst::String("K".into())));
    ir.add_expr(IrExpr::New {
        internal: owner,
        args: vec![first, second],
        ctor_params: Some(vec![Ty::Int, Ty::String]),
        ctor_desc: None,
        external_target: Some(target),
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    let frozen = CheckedBackendCallables::freeze(&ir, &libraries).expect("frozen selection");
    let fact = frozen.callable(target).expect("the selected constructor");
    assert_eq!(fact.kind, ExternalCallableKind::Constructor);
    assert_eq!(fact.ret, Ty::Unit);
    assert!(fact.generic_sig.is_none());
    assert_eq!(fact.klib_body_class_contract.as_ref(), Some(contract));
    assert_eq!(
        fact.klib_body_callable().unwrap().class_contract(),
        Some(contract)
    );

    let mut foreign = identity.clone();
    foreign.module.push_str("-foreign");
    assert!(libraries
        .external_callable_by_dependency(&foreign)
        .is_none());
    let KlibDeclarationSignature::Public(signature) = &identity.declaration else {
        panic!("a constructor has a public signature");
    };
    let changed = different_mask(signature);
    assert_eq!(changed.package(), signature.package());
    assert_eq!(changed.declaration(), signature.declaration());
    assert_eq!(changed.member_id(), signature.member_id());
    assert_ne!(changed.mask(), signature.mask());
    let mut wrong_mask = identity;
    wrong_mask.declaration = KlibDeclarationSignature::Public(changed);
    assert!(libraries
        .external_callable_by_dependency(&wrong_mask)
        .is_none());
}
