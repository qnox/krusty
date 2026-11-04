//! A getter leaf authenticates its property slot and complete declaring-class receiver.

use super::*;
use crate::libraries::{ExternalCallableRealization, KlibAccessorRole};
use crate::metadata::klib_ir::tree::{KlibIrProperty, KlibIrTypeArgument, KlibIrVariance};

#[derive(Clone, Copy, Debug)]
enum Change {
    None,
    ReceiverMask,
    ReceiverKind,
    ReceiverApplication,
    PropertyGroup,
    SetterSlot,
    Result,
}

fn pair_getter(libraries: &KlibLibraries) -> ExternalCallableRealization {
    let class = libraries.classifier(type_name("kotlin/Pair")).unwrap();
    let getter = &class.declared_callables["second"].properties()[0].getter;
    libraries
        .external_callable(getter.external_identity.unwrap())
        .unwrap()
}

/// Retain the real selected declaration and rebuild a body-free header in a fresh arena. No
/// executable implementation is available to hide a malformed property or receiver contract.
fn getter_header(
    libraries: &KlibLibraries,
    provider: &ExternalCallableRealization,
    change: Change,
) -> KlibDeclarationBodies {
    let selected = provider.klib_body_callable().unwrap();
    let contract = selected.class_contract().unwrap();
    let raw = libraries
        .declaration_bodies()
        .class_member_header(selected.identity(), &contract.signature)
        .unwrap()
        .unwrap();
    let mut class = raw.class.clone();
    let mut function = raw.function.clone();
    let mut arena = KlibIrArena::default();
    let any = arena.push_type(super::demo_package::T::NullableAny.klib());
    assert_eq!(class.type_parameters.len(), 2);
    let formals = class
        .type_parameters
        .iter_mut()
        .map(|parameter| {
            parameter.supertypes = vec![any];
            arena.push_type(KlibIrType::Simple {
                classifier: parameter.base.symbol.clone(),
                nullability: KlibIrNullability::NotSpecified,
                arguments: Vec::new(),
                annotations: Vec::new(),
                abbreviation: None,
            })
        })
        .collect::<Vec<_>>();
    let receiver_signature = if matches!(change, Change::ReceiverMask) {
        super::constructor_contracts::different_mask(&contract.signature)
    } else {
        contract.signature.clone()
    };
    let receiver = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(
            if matches!(change, Change::ReceiverKind) {
                KlibIrSymbolKind::Function
            } else {
                KlibIrSymbolKind::Class
            },
            receiver_signature,
        ),
        nullability: KlibIrNullability::NotSpecified,
        arguments: [
            formals[0],
            if matches!(change, Change::ReceiverApplication) {
                formals[0]
            } else {
                formals[1]
            },
        ]
        .into_iter()
        .map(|ty| KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty,
        })
        .collect(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    function.dispatch_receiver.as_mut().unwrap().ty = receiver;
    function.return_type = if matches!(change, Change::Result) {
        any
    } else {
        formals[1]
    };
    function.body = None;
    function.overridden.clear();
    let function = arena.push_function(function);
    let KlibDeclarationSignature::Accessor(accessor) = selected.signature() else {
        panic!("getter")
    };
    class.members = vec![KlibIrMember::Property(KlibIrProperty {
        base: base(public_symbol(
            KlibIrSymbolKind::Property,
            if matches!(change, Change::PropertyGroup) {
                contract.signature.clone()
            } else {
                accessor.property().clone()
            },
        )),
        name: "second".to_owned(),
        backing_field: None,
        getter: (!matches!(change, Change::SetterSlot)).then_some(function),
        setter: matches!(change, Change::SetterSlot).then_some(function),
    })];
    class.supertypes.clear();
    if let Some(this) = &mut class.this_receiver {
        this.ty = receiver;
    }
    let class = arena.push_class(class);
    KlibDeclarationBodies::from_libraries(vec![(
        contract.module.clone(),
        KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Class(class),
        }])
        .unwrap(),
    )])
    .unwrap()
}

#[test]
fn getter_leaves_reject_malformed_class_and_property_headers() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_getter(&libraries);
    let selected = provider.klib_body_callable().unwrap();
    let linked = super::super::accessor_calls::authenticate(
        selected,
        &getter_header(&libraries, &provider, Change::None),
    )
    .unwrap()
    .unwrap();
    assert_eq!(linked.property, selected.accessor().unwrap().property);
    assert_eq!(linked.ret, selected.ret());
    assert!(!linked.write);
    for change in [
        Change::ReceiverMask,
        Change::ReceiverKind,
        Change::ReceiverApplication,
        Change::PropertyGroup,
        Change::SetterSlot,
        Change::Result,
    ] {
        assert!(
            matches!(
                super::super::accessor_calls::authenticate(
                    selected,
                    &getter_header(&libraries, &provider, change)
                ),
                Err(KlibBodyDeclineReason::SignatureMismatch(_))
            ),
            "{change:?}"
        );
    }
}

#[test]
fn getter_facets_cannot_turn_public_functions_or_setters_into_getters() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_getter(&libraries);
    let bodies = getter_header(&libraries, &provider, Change::None);
    let mut public = provider.clone();
    let signature = public
        .klib_body_class_contract
        .as_ref()
        .unwrap()
        .signature
        .clone();
    public.dependency_callable.as_mut().unwrap().declaration =
        KlibDeclarationSignature::Public(signature.clone());
    public.declaration_signature = Some(KlibDeclarationSignature::Public(signature));
    assert!(
        matches!(super::super::accessor_calls::authenticate(public.klib_body_callable().unwrap(), &bodies),
        Err(KlibBodyDeclineReason::SignatureMismatch(ref reason))
            if reason == "a getter contract belongs to a non-accessor declaration")
    );
    let mut setter = provider.clone();
    setter.klib_body_accessor.as_mut().unwrap().role = KlibAccessorRole::Setter;
    assert!(super::super::accessor_calls::authenticate(
        setter.klib_body_callable().unwrap(),
        &bodies
    )
    .unwrap()
    .is_none());
}

#[test]
fn target_getter_capability_requires_a_unique_accessor_contract() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_getter(&libraries);
    let selected = provider.klib_body_callable().unwrap();
    let bodies = getter_header(&libraries, &provider, Change::None);
    let facts = KlibCalleeFacts::new([selected]);
    assert!(!facts
        .target_getter(selected.identity(), &bodies, &|_| false)
        .unwrap());
    assert!(facts
        .target_getter(selected.identity(), &bodies, &|_| true)
        .unwrap());
    for role_change in [false, true] {
        let mut other = provider.clone();
        let facet = other.klib_body_accessor.as_mut().unwrap();
        if role_change {
            facet.role = KlibAccessorRole::Setter;
        } else {
            facet.property = crate::fir::ExternalPropertyId::from_raw(facet.property.raw() + 1);
        }
        let ambiguous = KlibCalleeFacts::new([selected, other.klib_body_callable().unwrap()]);
        assert!(
            matches!(ambiguous.target_getter(selected.identity(), &bodies, &|_| true),
            Err(KlibBodyDeclineReason::SignatureMismatch(ref reason))
                if reason == "conflicting provider contracts for a getter declaration")
        );
    }
}
