//! Allocation entries remain distinct from both instance initialization and ordinary bodies.

use super::demo_package::{demo, operator, T};
use super::*;
use crate::backend::{DependencyExportSignatures, ExportedConstructor};
use crate::libraries::{KlibBodyClassContract, KlibBodyClassParameters, TypeKind};
use crate::metadata::klib_ir::tree::{KlibIrClass, KlibIrField};
use crate::metadata::semantic::KotlinModality;

struct Fixture {
    constructor: BackendCallableFact,
    factory: BackendCallableFact,
    bodies: KlibDeclarationBodies,
}

fn function(
    symbol: KlibIrSymbol,
    name: &str,
    parameters: Vec<KlibIrParameter>,
    result: KlibIrTypeId,
    body: Option<Vec<KlibIrStatement>>,
) -> KlibIrFunction {
    KlibIrFunction {
        base: KlibIrDeclarationBase {
            flags: 6,
            ..base(symbol)
        },
        name: name.into(),
        constructor: name == "<init>",
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: parameters,
        return_type: result,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: body.map(KlibIrBody::Block),
    }
}

fn fixture(bad_initializer: bool) -> Fixture {
    let prototype = demo(&[("seed", &[T::Int, T::Int], T::Unit)], |_, _| {
        Some(Vec::new())
    });
    let owner = type_name("demo/Act");
    let signature = operator(
        &["demo"],
        Some("Act"),
        "<init>",
        &[T::Int.kotlin(), T::Int.kotlin()],
    )
    .0;
    let any_signature = operator(&["kotlin"], Some("Any"), "<init>", &[]).0;
    let identity = KlibDependencyCallable {
        module: "demo".into(),
        declaration: KlibDeclarationSignature::Public(signature.clone()),
    };
    let mut constructor = prototype.facts["seed"].clone();
    constructor.name = "<init>".into();
    constructor.kind = ExternalCallableKind::Constructor;
    constructor.visibility = Visibility::Public;
    constructor.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(owner));
    constructor.declaration_signature = Some(identity.declaration.clone());
    constructor.dependency_callable = Some(identity.clone());
    constructor.klib_body_class_contract = Some(KlibBodyClassContract {
        module: "demo".into(),
        signature: KlibPublicIdSignature::classifier(owner),
        classifier: owner,
        parameters: KlibBodyClassParameters {
            formals: Box::default(),
            bounds: Box::default(),
            variances: Box::default(),
            own_count: 0,
        },
        kind: TypeKind::Class,
        modality: KotlinModality::Final,
        is_value: false,
        is_inner: false,
        is_expect: false,
        primary_constructor: Some(signature.clone()),
    });
    let mut arena = KlibIrArena::default();
    let int = arena.push_type(T::Int.klib());
    let unit = arena.push_type(T::Unit.klib());
    let any = arena.push_type(T::Any.klib());
    let class_symbol = public_symbol(
        KlibIrSymbolKind::Class,
        KlibPublicIdSignature::classifier(owner),
    );
    let own_type = arena.push_type(KlibIrType::Simple {
        classifier: class_symbol.clone(),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let parameters = ["a", "b"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| KlibIrParameter {
            base: base(local_symbol(KlibIrSymbolKind::ValueParameter, i as u32)),
            name: name.into(),
            ty: int,
            vararg_element_type: None,
            default_value: None,
        })
        .collect::<Vec<_>>();
    let fields = parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let value = arena.push_expr(
                Some(int),
                KlibIrExprKind::GetValue {
                    symbol: parameter.base.symbol.clone(),
                    origin: None,
                },
            );
            KlibIrMember::Field(KlibIrField {
                base: base(local_symbol(KlibIrSymbolKind::Field, 10 + index as u32)),
                name: format!("saved{index}"),
                ty: int,
                initializer: Some(value),
            })
        })
        .collect::<Vec<_>>();
    let delegate = arena.push_expr(
        Some(unit),
        KlibIrExprKind::DelegatingConstructorCall(KlibIrMemberAccess {
            symbol: public_symbol(KlibIrSymbolKind::Constructor, any_signature.clone()),
            arguments: KlibIrArguments::Flat(Vec::new()),
            type_arguments: Vec::new(),
            origin: None,
        }),
    );
    let initialize = arena.push_expr(
        Some(unit),
        KlibIrExprKind::InstanceInitializerCall(class_symbol.clone()),
    );
    let constructor_function = arena.push_function(function(
        public_symbol(KlibIrSymbolKind::Constructor, signature),
        "<init>",
        parameters.clone(),
        own_type,
        Some(vec![
            KlibIrStatement::Expression(delegate),
            KlibIrStatement::Expression(initialize),
        ]),
    ));
    let mut members = vec![KlibIrMember::Function(constructor_function)];
    members.extend(fields);
    if bad_initializer {
        let missing = arena.push_expr(Some(unit), KlibIrExprKind::Missing);
        members.push(KlibIrMember::AnonymousInitializer {
            base: base(local_symbol(KlibIrSymbolKind::AnonymousInit, 20)),
            statements: vec![KlibIrStatement::Expression(missing)],
        });
    }
    let class = arena.push_class(KlibIrClass {
        base: base(class_symbol),
        name: "Act".into(),
        this_receiver: Some(KlibIrParameter {
            base: base(local_symbol(KlibIrSymbolKind::ReceiverParameter, 2)),
            name: "<this>".into(),
            ty: own_type,
            vararg_element_type: None,
            default_value: None,
        }),
        type_parameters: Vec::new(),
        supertypes: vec![any],
        members,
        value_class_representation: None,
        sealed_subclasses: Vec::new(),
    });
    let class_tree = KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Class(class),
    };
    let mut any_arena = KlibIrArena::default();
    let any_type = any_arena.push_type(T::Any.klib());
    let any_constructor = any_arena.push_function(function(
        public_symbol(KlibIrSymbolKind::Constructor, any_signature),
        "<init>",
        Vec::new(),
        any_type,
        None,
    ));
    let any_class = any_arena.push_class(KlibIrClass {
        base: base(public_symbol(
            KlibIrSymbolKind::Class,
            KlibPublicIdSignature::classifier(crate::types::wk::any()),
        )),
        name: "Any".into(),
        this_receiver: None,
        type_parameters: Vec::new(),
        supertypes: Vec::new(),
        members: vec![KlibIrMember::Function(any_constructor)],
        value_class_representation: None,
        sealed_subclasses: Vec::new(),
    });
    let any_tree = KlibIrDeclarationTree {
        arena: any_arena,
        declaration: KlibIrMember::Class(any_class),
    };
    let mut arena = KlibIrArena::default();
    let int = arena.push_type(T::Int.klib());
    let own_type = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(
            KlibIrSymbolKind::Class,
            KlibPublicIdSignature::classifier(owner),
        ),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let arguments = parameters
        .iter()
        .map(|parameter| {
            Some(arena.push_expr(
                Some(int),
                KlibIrExprKind::GetValue {
                    symbol: parameter.base.symbol.clone(),
                    origin: None,
                },
            ))
        })
        .collect();
    let value = arena.push_expr(
        Some(own_type),
        KlibIrExprKind::ConstructorCall {
            access: KlibIrMemberAccess {
                symbol: public_symbol(
                    KlibIrSymbolKind::Constructor,
                    match &identity.declaration {
                        KlibDeclarationSignature::Public(s) => s.clone(),
                        _ => unreachable!(),
                    },
                ),
                arguments: KlibIrArguments::Flat(arguments),
                type_arguments: Vec::new(),
                origin: None,
            },
            constructor_type_arguments: 0,
        },
    );
    let factory_signature =
        operator(&["demo"], None, "make", &[T::Int.kotlin(), T::Int.kotlin()]).0;
    let factory_symbol = public_symbol(KlibIrSymbolKind::Function, factory_signature.clone());
    let nothing = arena.push_type(T::Nothing.klib());
    let ret = arena.push_expr(
        Some(nothing),
        KlibIrExprKind::Return {
            target: factory_symbol.clone(),
            value,
        },
    );
    let mut factory_parameters = parameters;
    for parameter in &mut factory_parameters {
        parameter.ty = int;
    }
    let factory_function = arena.push_function(function(
        factory_symbol,
        "make",
        factory_parameters,
        own_type,
        Some(vec![KlibIrStatement::Expression(ret)]),
    ));
    let factory_tree = KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(factory_function),
    };
    let mut factory = prototype.facts["seed"].clone();
    factory.name = "make".into();
    factory.visibility = Visibility::Public;
    factory.ret = Ty::obj_name(owner);
    factory.dependency_callable = Some(KlibDependencyCallable {
        module: "demo".into(),
        declaration: KlibDeclarationSignature::Public(factory_signature),
    });
    factory.declaration_signature = factory
        .dependency_callable
        .as_ref()
        .map(|i| i.declaration.clone());
    let bodies = KlibDeclarationBodies::from_libraries(vec![(
        "demo".into(),
        KlibIrModuleTrees::from_trees(vec![class_tree, any_tree, factory_tree]).unwrap(),
    )])
    .unwrap();
    Fixture {
        constructor,
        factory,
        bodies,
    }
}

#[test]
fn constructor_root_exports_allocation_with_exact_parameters_and_declared_self_type() {
    let f = fixture(false);
    let selected = f.constructor.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::new([selected]);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_root_with_intrinsics_and_externals(
        selected,
        &f.bodies,
        &facts,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(
        unit.callable_functions().count(),
        0,
        "private instance initialization is not an ordinary export"
    );
    let constructors = unit
        .allocating_constructors()
        .map(|(i, c)| (i.clone(), c))
        .collect::<Vec<_>>();
    let [(identity, class)] = constructors.as_slice() else {
        panic!("one constructor")
    };
    let (identity, class) = (identity.clone(), *class);
    assert_eq!(&identity, selected.identity());
    let mut ir = unit.into_ir();
    let owner = type_name("demo/Act");
    assert_eq!(
        ir.declared_class_self_type(owner),
        Some(Ty::obj_name(owner))
    );
    let stores = ir
        .exprs
        .iter()
        .filter_map(|e| match e {
            IrExpr::SetField {
                class: c,
                index: field,
                receiver,
                value,
            } if *c == class => Some((*field, *receiver, *value)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores.len(), 2);
    for (index, (field, receiver, value)) in stores.iter().enumerate() {
        assert_eq!(*field, index as u32);
        assert!(matches!(ir.expr(*receiver), IrExpr::GetValue(0)));
        assert!(matches!(ir.expr(*value), IrExpr::GetValue(slot) if *slot == index as u32 + 1));
    }
    let mut exports = DependencyExportSignatures::default();
    exports.record_constructor(
        ExportedConstructor {
            class,
            secondary: None,
        },
        identity.clone(),
    );
    crate::backend::construction_entries::realize(&mut ir, &mut exports);
    let published = exports.callables().collect::<Vec<_>>();
    let [(entry, exported)] = published.as_slice() else {
        panic!("one allocation entry")
    };
    assert_eq!(*exported, &identity);
    assert_eq!(ir.functions[*entry as usize].params, [Ty::Int, Ty::Int]);
    assert_eq!(ir.functions[*entry as usize].ret, Ty::obj_name(owner));
    assert_eq!(ir.construction_entries.get(entry), Some(&class));
    let constructions = ir
        .exprs
        .iter()
        .filter_map(|e| match e {
            IrExpr::New {
                internal,
                args,
                external_target,
                declared_params,
                ..
            } => Some((*internal, args, external_target, declared_params)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(internal, args, target, declared)] = constructions.as_slice() else {
        panic!("one allocating New")
    };
    assert_eq!(*internal, owner);
    assert_eq!(**target, None);
    assert_eq!(declared.as_deref(), Some(&[Ty::Int, Ty::Int][..]));
    assert_eq!(
        args.iter()
            .map(|e| match ir.expr(*e) {
                IrExpr::GetValue(slot) => *slot,
                other => panic!("entry operand: {other:?}"),
            })
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn constructor_allocation_export_does_not_guess_a_missing_self_type() {
    let f = fixture(false);
    let selected = f.constructor.klib_body_callable().unwrap();
    let mut unit = DependencyBodyUnit::default();
    unit.lower_root_with_intrinsics_and_externals(
        selected,
        &f.bodies,
        &KlibCalleeFacts::new([selected]),
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &Default::default(),
    )
    .unwrap();
    let (_, class) = unit.allocating_constructors().next().unwrap();
    let mut ir = unit.into_ir();
    ir.restore_declared_class_self_type_owners(&Default::default());
    let before = (ir.exprs.len(), ir.functions.len());
    let mut exports = DependencyExportSignatures::default();
    exports.record_constructor(
        ExportedConstructor {
            class,
            secondary: None,
        },
        selected.identity().clone(),
    );
    crate::backend::construction_entries::realize(&mut ir, &mut exports);
    assert_eq!(exports.callables().count(), 0);
    assert!(ir.construction_entries.is_empty());
    assert_eq!((ir.exprs.len(), ir.functions.len()), before);
}

#[test]
fn declined_constructor_initializer_rolls_back_ir_layout_and_declared_self_type() {
    let f = fixture(true);
    let selected = f.constructor.klib_body_callable().unwrap();
    let mut unit = DependencyBodyUnit::default();
    let kept = demo(&[("kept", &[], T::Unit)], |_, _| Some(Vec::new()));
    unit.lower_function(
        kept.callable("kept"),
        &kept.bodies,
        &kept.callees(&["kept"]),
    )
    .unwrap();
    let before = (unit.ir().exprs.len(), unit.ir().functions.len());
    let error = unit
        .lower_root_with_intrinsics_and_externals(
            selected,
            &f.bodies,
            &KlibCalleeFacts::new([selected]),
            crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &Default::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation("an omitted argument")
    );
    assert_eq!((unit.ir().exprs.len(), unit.ir().functions.len()), before);
    assert_eq!(
        unit.callable_functions()
            .map(|(identity, _)| identity)
            .collect::<Vec<_>>(),
        [kept.callable("kept").identity()]
    );
    assert!(unit.ir().classes.is_empty());
    assert_eq!(
        unit.ir().declared_class_self_type(type_name("demo/Act")),
        None
    );
    assert_eq!(unit.allocating_constructors().count(), 0);
}

#[test]
fn later_serialized_constructor_call_reuses_only_an_authenticated_allocation_export() {
    let f = fixture(false);
    let constructor = f.constructor.klib_body_callable().unwrap();
    let factory = f.factory.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::new([constructor, factory]);
    let target = crate::fir::ExternalCallableId::from_raw(7);
    for (handle, succeeds, owns_layout) in [
        (
            super::super::EmittedDependencyCallable::allocating_constructor(target),
            true,
            false,
        ),
        (
            super::super::EmittedDependencyCallable::constructor(target),
            true,
            true,
        ),
        (
            super::super::EmittedDependencyCallable::new(target, None, None),
            false,
            false,
        ),
    ] {
        let mut unit = DependencyBodyUnit::default();
        let externals =
            crate::fx_hash::HashMap::from_iter([(constructor.identity().clone(), handle)]);
        let result = unit.lower_function_with_intrinsics_and_externals(
            factory,
            &f.bodies,
            &facts,
            crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &externals,
        );
        if !succeeds {
            assert_eq!(
                result.unwrap_err().reason(),
                &KlibBodyDeclineReason::SignatureMismatch(
                    "an authenticated constructor has no exact constructor external handle".into()
                )
            );
            assert!(unit.ir().functions.is_empty());
            continue;
        }
        result.unwrap();
        assert_eq!(
            unit.ir().classes.len(),
            usize::from(owns_layout),
            "only a previously published allocation entry owns the physical class elsewhere"
        );
        let news = unit
            .ir()
            .exprs
            .iter()
            .filter_map(|e| match e {
                IrExpr::New {
                    external_target,
                    internal,
                    ..
                } => Some((*external_target, *internal)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(news, [(Some(target), type_name("demo/Act"))]);
    }
}

#[test]
fn rollback_preserves_a_preexisting_self_type_without_class_storage() {
    let retained = type_name("demo/Retained");
    let appended = type_name("demo/Appended");
    let mut ir = IrFile::default();
    ir.record_declared_class_self_type(retained, Ty::obj_name(retained));
    let checkpoint = super::super::lowering_checkpoint::LoweringCheckpoint::of(&ir);
    ir.add_class(crate::ir::IrClass::synthetic(retained));
    ir.record_declared_class_self_type(retained, Ty::obj_name(retained));
    ir.add_class(crate::ir::IrClass::synthetic(appended));
    ir.record_declared_class_self_type(appended, Ty::obj_name(appended));
    checkpoint.restore(&mut ir);
    assert!(ir.classes.is_empty());
    assert_eq!(
        ir.declared_class_self_type(retained),
        Some(Ty::obj_name(retained))
    );
    assert_eq!(ir.declared_class_self_type(appended), None);
}
