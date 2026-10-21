//! Provider operations must survive both the declaration and checked-IR handoffs.

use super::*;
use crate::libraries::{CompilerIntrinsic, PrimitiveUnaryIntrinsic};
use crate::metadata::klib_ir::tree::{
    KlibIrClass, KlibIrProperty, KlibIrTypeArgument, KlibIrVariance,
};

#[derive(Clone, Copy, Debug)]
enum RawChange {
    None,
    ReceiverMask,
    ClassMask,
    ForeignFormal,
    Bound,
    Variance,
    Reified,
    AccessorGroup,
    AccessorSlot,
    Result,
    Layout,
    DuplicateClass,
}

/// Rebuild just the authenticated header, with no implementation to fall back to. The production
/// provider and original class formals come from the real selected distribution.
fn array_header_fixture(
    libraries: &KlibLibraries,
    provider: &crate::libraries::ExternalCallableRealization,
    change: RawChange,
    caller: Option<KlibIrDeclarationTree>,
) -> KlibDeclarationBodies {
    let selected = provider.klib_body_callable().unwrap();
    let contract = selected.class_contract().unwrap();
    let member = libraries
        .declaration_bodies()
        .primitive_member_header(selected.identity(), &contract.signature)
        .unwrap()
        .unwrap();
    let mut class: KlibIrClass = member.class.clone();
    let mut function = member.function.clone();
    let mut arena = KlibIrArena::default();
    let int = arena.push_type(super::demo_package::T::Int.klib());
    let any = arena.push_type(super::demo_package::T::NullableAny.klib());
    assert_eq!(class.type_parameters.len(), 1);
    class.type_parameters[0].supertypes = vec![any];
    let wrong_mask = crate::metadata::id_signature::decode_public_id_signature(
        &[10, 6, 8, 0, 16, 1, 32, 1],
        &["kotlin".to_owned(), "Array".to_owned()],
    )
    .unwrap()
    .unwrap();
    assert_ne!(wrong_mask, contract.signature);
    let formal = if matches!(change, RawChange::ForeignFormal) {
        local_symbol(KlibIrSymbolKind::TypeParameter, 900)
    } else {
        class.type_parameters[0].base.symbol.clone()
    };
    let formal = arena.push_type(KlibIrType::Simple {
        classifier: formal,
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let dispatch = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(
            KlibIrSymbolKind::Class,
            if matches!(change, RawChange::ReceiverMask) {
                wrong_mask.clone()
            } else {
                contract.signature.clone()
            },
        ),
        nullability: KlibIrNullability::NotSpecified,
        arguments: vec![KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty: formal,
        }],
        annotations: Vec::new(),
        abbreviation: None,
    });
    function.dispatch_receiver.as_mut().unwrap().ty = dispatch;
    function.return_type = int;
    function.body = None;
    function.overridden.clear();
    match change {
        RawChange::ClassMask => class.base.symbol.signature = KlibIrSignature::Public(wrong_mask),
        RawChange::Bound => class.type_parameters[0].supertypes = vec![int],
        RawChange::Variance => class.type_parameters[0].base.flags = 2,
        RawChange::Reified => class.type_parameters[0].base.flags |= 8,
        RawChange::Result => function.return_type = any,
        RawChange::Layout => {
            function.extension_receiver = Some(KlibIrParameter {
                base: base(local_symbol(KlibIrSymbolKind::ValueParameter, 901)),
                name: "receiver".to_owned(),
                ty: int,
                vararg_element_type: None,
                default_value: None,
            })
        }
        _ => {}
    }
    let function = arena.push_function(function);
    let KlibDeclarationSignature::Accessor(accessor) = selected.signature() else {
        panic!("getter")
    };
    let property_signature = if matches!(change, RawChange::AccessorGroup) {
        contract.signature.clone()
    } else {
        accessor.property().clone()
    };
    class.members = vec![KlibIrMember::Property(KlibIrProperty {
        base: base(public_symbol(
            KlibIrSymbolKind::Property,
            property_signature,
        )),
        name: "size".to_owned(),
        backing_field: None,
        getter: (!matches!(change, RawChange::AccessorSlot)).then_some(function),
        setter: matches!(change, RawChange::AccessorSlot).then_some(function),
    })];
    class.this_receiver.as_mut().unwrap().ty = dispatch;
    let duplicate = class.clone();
    let mut id = arena.push_class(class);
    if matches!(change, RawChange::DuplicateClass) {
        let duplicate = arena.push_class(duplicate);
        // A second exact header can be nested without introducing a second function identity.
        let mut outer = arena.class(id).clone();
        outer.members.push(KlibIrMember::Class(duplicate));
        id = arena.push_class(outer);
    }
    let trees = std::iter::once(KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Class(id),
    })
    .chain(caller)
    .collect();
    KlibDeclarationBodies::from_libraries(vec![(
        contract.module.clone(),
        KlibIrModuleTrees::from_trees(trees).unwrap(),
    )])
    .unwrap()
}

#[test]
fn klib_array_primitive_rejects_malformed_raw_class_and_accessor_headers() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let class = libraries.classifier(type_name("kotlin/Array")).unwrap();
    let target = class.declared_callables["size"].properties()[0]
        .getter
        .external_identity
        .unwrap();
    let provider = libraries.external_callable(target).unwrap();
    let selected = provider.klib_body_callable().unwrap();
    let valid = array_header_fixture(&libraries, &provider, RawChange::None, None);
    assert!(
        super::super::primitive_calls::authenticate(selected, &valid)
            .unwrap()
            .is_some()
    );
    for change in [
        RawChange::ReceiverMask,
        RawChange::ClassMask,
        RawChange::ForeignFormal,
        RawChange::Bound,
        RawChange::Variance,
        RawChange::Reified,
        RawChange::AccessorGroup,
        RawChange::AccessorSlot,
        RawChange::Result,
        RawChange::Layout,
        RawChange::DuplicateClass,
    ] {
        let bodies = array_header_fixture(&libraries, &provider, change, None);
        assert!(
            super::super::primitive_calls::authenticate(selected, &bodies).is_err(),
            "{change:?}"
        );
    }
}

fn array_caller(
    provider: &crate::libraries::ExternalCallableRealization,
) -> (BackendCallableFact, KlibIrDeclarationTree) {
    use super::demo_package::{declaration, T};
    let primitive = provider.klib_body_callable().unwrap();
    let mut function = declaration(("sizeOf", &[T::Int], T::Int));
    function.params = vec![KotlinType::Class {
        internal: "kotlin/Array".to_owned(),
        args: vec![T::Int.kotlin()],
        nullable: false,
        shape: KotlinFunctionTypeShape::default(),
    }];
    let libraries = KlibLibraries::from_named_packages(
        &primitive.identity().module,
        vec![(
            vec!["demo".to_owned()],
            KotlinPackage {
                functions: vec![function],
                ..KotlinPackage::default()
            },
        )],
    )
    .unwrap();
    let fact = frozen_function(
        &libraries,
        "demo",
        "sizeOf",
        None,
        &[Ty::obj_args_name(
            primitive.class_contract().unwrap().classifier,
            &[Ty::Int],
        )],
    );
    let KlibDeclarationSignature::Public(signature) =
        &fact.dependency_callable.as_ref().unwrap().declaration
    else {
        panic!("function")
    };
    let symbol = public_symbol(KlibIrSymbolKind::Function, signature.clone());
    let mut arena = KlibIrArena::default();
    let int = arena.push_type(T::Int.klib());
    let array = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(
            KlibIrSymbolKind::Class,
            primitive.class_contract().unwrap().signature.clone(),
        ),
        nullability: KlibIrNullability::NotSpecified,
        arguments: vec![KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty: int,
        }],
        annotations: Vec::new(),
        abbreviation: None,
    });
    let parameter = local_symbol(KlibIrSymbolKind::ValueParameter, 800);
    let receiver = arena.push_expr(
        Some(array),
        KlibIrExprKind::GetValue {
            symbol: parameter.clone(),
            origin: None,
        },
    );
    let KlibDeclarationSignature::Accessor(accessor) = primitive.signature() else {
        panic!("accessor")
    };
    let call = arena.push_expr(
        Some(int),
        KlibIrExprKind::Call {
            access: KlibIrMemberAccess {
                symbol: KlibIrSymbol {
                    kind: KlibIrSymbolKind::Function,
                    signature: KlibIrSignature::Accessor(accessor.clone()),
                },
                arguments: KlibIrArguments::Flat(vec![Some(receiver)]),
                type_arguments: Vec::new(),
                origin: None,
            },
            super_qualifier: None,
        },
    );
    let nothing = arena.push_type(T::Nothing.klib());
    let returned = arena.push_expr(
        Some(nothing),
        KlibIrExprKind::Return {
            target: symbol.clone(),
            value: call,
        },
    );
    let id = arena.push_function(KlibIrFunction {
        base: base(symbol),
        name: "sizeOf".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: vec![KlibIrParameter {
            base: base(parameter),
            name: "a".to_owned(),
            ty: array,
            vararg_element_type: None,
            default_value: None,
        }],
        return_type: int,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(
            returned,
        )])),
    });
    (
        fact,
        KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Function(id),
        },
    )
}

#[test]
fn klib_primitive_planning_and_body_lowering_use_the_same_authenticated_shape() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let class = libraries.classifier(type_name("kotlin/Array")).unwrap();
    let provider = libraries
        .external_callable(
            class.declared_callables["size"].properties()[0]
                .getter
                .external_identity
                .unwrap(),
        )
        .unwrap();
    let primitive = provider.klib_body_callable().unwrap();
    let (caller, tree) = array_caller(&provider);
    let root = caller.klib_body_callable().unwrap();
    let bodies = array_header_fixture(&libraries, &provider, RawChange::None, Some(tree));
    let facts = KlibCalleeFacts::with_declarations([root], [primitive]);
    assert!(facts
        .common_primitive(primitive.identity(), &bodies)
        .unwrap()
        .is_some());
    let mut unit = DependencyBodyUnit::default();
    let function = unit.lower_function(root, &bodies, &facts).unwrap();
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, function),
        super::demo_package::source_body(
            "fun sizeOf(a: Array<Int>): Int { return a.size }",
            "sizeOf",
            1,
        )
    );
    assert_eq!(
        unit.ir().functions.len(),
        1,
        "no primitive implementation is emitted"
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => Some((&**callee, dispatch_receiver.is_some(), args.len())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        [(
            &Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::ArraySize,
                ret: Ty::Int
            },
            true,
            0
        )]
    );
    let selected = KlibCalleeFacts::new([root, primitive]);
    assert!(selected
        .common_primitive(primitive.identity(), &bodies)
        .unwrap()
        .is_none());
    let mut conflicting = provider.clone();
    conflicting.callable.ret = Ty::Boolean;
    let conflict = KlibCalleeFacts::with_declarations(
        [root],
        [primitive, conflicting.klib_body_callable().unwrap()],
    );
    assert!(conflict
        .common_primitive(primitive.identity(), &bodies)
        .is_err());
    let mut ordinary = provider.clone();
    ordinary.callable.compiler_intrinsic = None;
    ordinary.callable.member_realization = crate::libraries::MemberRealization::Dispatch;
    let ordinary =
        KlibCalleeFacts::with_declarations([root], [ordinary.klib_body_callable().unwrap()]);
    assert!(ordinary
        .common_primitive(primitive.identity(), &bodies)
        .unwrap()
        .is_none());
    assert!(DependencyBodyUnit::default()
        .lower_function(root, &bodies, &ordinary)
        .is_err());
}

#[test]
fn klib_member_primitives_survive_both_frozen_views() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    for (owner, name, property, expected) in [
        ("kotlin/Array", "size", true, CompilerIntrinsic::ArraySize),
        (
            "kotlin/Float",
            "unaryMinus",
            false,
            CompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Negate),
        ),
    ] {
        let class = libraries
            .classifier(type_name(owner))
            .expect("stdlib class");
        let candidates = &class.declared_callables[name];
        let target = if property {
            candidates.properties()[0].getter.external_identity
        } else {
            candidates.functions()[0].callable.external_identity
        }
        .expect("provider identity");
        let provider = libraries
            .external_callable(target)
            .expect("exact declaration");
        assert_eq!(
            provider.klib_body_callable().unwrap().compiler_intrinsic(),
            Some(expected)
        );
        let proof = super::super::primitive_calls::authenticate(
            provider.klib_body_callable().unwrap(),
            libraries.declaration_bodies(),
        )
        .unwrap_or_else(|reason| panic!("{owner}.{name}: {reason:?}"));
        assert!(proof.is_some());
        for mutation in 0..5 {
            let mut bad = provider.clone();
            match mutation {
                0 => bad
                    .klib_body_class_contract
                    .as_mut()
                    .unwrap()
                    .module
                    .push_str("-foreign"),
                1 => bad.callable.ret = Ty::Boolean,
                2 => bad.callable.context_count = 1,
                3 => {
                    bad.klib_body_class_contract
                        .as_mut()
                        .unwrap()
                        .parameters
                        .own_count += 1
                }
                _ => bad.klib_body_class_contract = None,
            }
            assert!(
                super::super::primitive_calls::authenticate(
                    bad.klib_body_callable().unwrap(),
                    libraries.declaration_bodies(),
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let mut ir = IrFile::default();
        ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::External {
                target,
                default_provider: None,
                params: provider.callable.params.clone(),
                ret: provider.callable.ret,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            }),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let frozen = CheckedBackendCallables::freeze(&ir, &libraries).expect("frozen");
        assert_eq!(
            frozen
                .callable(target)
                .unwrap()
                .klib_body_callable()
                .unwrap()
                .compiler_intrinsic(),
            Some(expected)
        );
    }
}
