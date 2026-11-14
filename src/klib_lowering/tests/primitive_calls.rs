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
        .class_member_header(
            selected.identity(),
            &contract.signature,
            Some(crate::libraries::KlibAccessorRole::Getter),
        )
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
    let target = class.declared_callables["size"].properties()[0]
        .getter
        .external_identity
        .unwrap();
    let provider = libraries.external_callable(target).unwrap();
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
    let accessor = primitive.accessor().unwrap().property;
    let mut externals = crate::fx_hash::HashMap::default();
    externals.insert(
        primitive.identity().clone(),
        super::super::EmittedDependencyCallable::target_realization(
            target,
            CompilerIntrinsic::ArraySize,
        ),
    );
    let mut external_unit = DependencyBodyUnit::default();
    let external_function = external_unit
        .lower_function_with_intrinsics_and_externals(
            root,
            &bodies,
            &facts,
            &crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &externals,
        )
        .unwrap();
    validated(external_unit.ir());
    assert_eq!(
        lowered_body(&external_unit, external_function),
        lowered_body(&unit, function),
        "a target-owned Array.size leaf keeps the checked property-read lowering"
    );
    assert_eq!(
        external_unit
            .ir()
            .exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::Checked(operation) if matches!(&**operation, crate::ir::IrCheckedOperation::ExternalPropertyRead { target: found, .. } if *found == accessor)))
            .count(),
        1,
        "the authenticated getter identity remains an external property read"
    );
    // The read stays the selected property's ordinary read; no call of an operation stands in.
    let reads = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call { .. } => panic!("a call in the lowered size read: {expression:?}"),
            IrExpr::Checked(operation) => match &**operation {
                crate::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    receiver,
                    arguments,
                    result,
                    ..
                } => Some((*target, receiver.is_some(), arguments.len(), *result)),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reads, [(accessor, true, 0, Ty::Int)]);
    // Each lowering reads its own provider's `Array.size`: the property whose getter is the array
    // size operation the provider attached to `kotlin.Array`.
    use crate::symbol_source::SymbolSource;
    let klib_property = libraries.external_property(accessor).unwrap();
    assert_eq!(
        (klib_property.name.as_str(), klib_property.getter),
        ("size", target)
    );
    let source = crate::fir_lower::tests::lower_single_source_with_platform(
        "fun sizeOf(a: Array<Int>): Int { return a.size }",
        "KlibBodyEquivalent",
        Box::new(crate::libraries::EmptySymbolSource),
    );
    let source_reads = source
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Checked(operation) => match &**operation {
                crate::ir::IrCheckedOperation::ExternalPropertyRead { target, .. } => Some(*target),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [source_property] = source_reads.as_slice() else {
        panic!("one source size read: {source_reads:?}")
    };
    let core = crate::libraries::EmptySymbolSource;
    let core_property = core.external_property(*source_property).unwrap();
    let core_getter = core.external_callable(core_property.getter).unwrap();
    assert_eq!(
        (
            core_property.name.as_str(),
            core_getter.callable.owner,
            core_getter.callable.compiler_intrinsic,
        ),
        (
            "size",
            type_name("kotlin/Array"),
            Some(CompilerIntrinsic::ArraySize),
        )
    );
    assert_eq!(
        (provider.callable.owner, primitive.compiler_intrinsic()),
        (
            type_name("kotlin/Array"),
            Some(CompilerIntrinsic::ArraySize)
        )
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
fn checked_klib_property_facts_use_the_exact_property_name() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let owner = type_name("kotlin/CharSequence");
    let classifier = libraries.classifier(owner).unwrap();
    let property = &classifier.declared_callables["length"].properties()[0];
    let identity = property.getter.external_identity.unwrap();
    let realization = libraries.external_callable(identity).unwrap();
    let selected = realization.klib_body_callable().unwrap();
    let checked = super::checked_backend_klib_facts(
        crate::resolve::for_checked_klib_bodies(
            &libraries,
            libraries.declaration_bodies(),
            [selected],
        )
        .expect("selected property body has checked facts"),
    );
    let facts = KlibCalleeFacts::new([selected]).with_checked_dependency_facts(&checked);
    let [implementation] = facts.builtin_implementation_facts()[&identity].as_ref() else {
        panic!("CharSequence.length has one represented builtin implementation");
    };
    assert_eq!(implementation.receiver, Ty::String);
    let declaration = implementation
        .declaration
        .as_ref()
        .expect("String provides its own CharSequence.length implementation");
    assert_eq!(declaration.operation, Some(CompilerIntrinsic::StringLength));
    assert!(declaration.params.is_empty());
    assert_eq!(declaration.ret, Ty::Int);

    let expected = facts.builtin_implementation_facts()[&identity].clone();
    let mut unit = DependencyBodyUnit::default();
    unit.record_builtin_implementation_facts(&facts).unwrap();
    assert_eq!(
        unit.ir().external_builtin_implementations[&identity],
        expected
    );
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

#[test]
fn an_external_array_size_leaf_keeps_its_authenticated_property_read() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let array = libraries
        .classifier(type_name("kotlin/Array"))
        .expect("the standard library declares Array");
    let target = array.declared_callables["size"].properties()[0]
        .getter
        .external_identity
        .expect("Array.size has an exact provider identity");
    let selected = libraries
        .external_callable(target)
        .expect("the exact Array.size getter")
        .klib_body_callable()
        .expect("the getter has a KLIB declaration contract");
    assert_eq!(
        selected.compiler_intrinsic(),
        Some(CompilerIntrinsic::ArraySize)
    );

    let facts = KlibCalleeFacts::new([selected]);
    let property_read = facts
        .target_property_read(
            selected.identity(),
            libraries.declaration_bodies(),
            CompilerIntrinsic::ArraySize,
        )
        .expect("the selected getter authenticates")
        .expect("Array.size is the exact target-owned property operation");
    assert!(matches!(
        property_read.operation,
        super::super::primitive_calls::CommonPrimitive::PropertyRead(CompilerIntrinsic::ArraySize)
    ));
    assert_eq!(
        property_read.property,
        selected.accessor().map(|accessor| accessor.property)
    );
    assert!(facts
        .target_property_read(
            selected.identity(),
            libraries.declaration_bodies(),
            CompilerIntrinsic::StringLength,
        )
        .expect("a different intrinsic is a nonmatch")
        .is_none());
    assert!(facts
        .target_property_read(
            selected.identity(),
            libraries.declaration_bodies(),
            CompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Negate),
        )
        .expect("ordinary target intrinsics are not property accessors")
        .is_none());
}
