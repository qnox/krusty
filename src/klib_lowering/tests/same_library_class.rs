//! A body may use a non-public class of the library that declares the body. The same file-local
//! identity in another library, even one the producer depends on, stays refused.

use super::demo_package::{demo_with, selected_demo_module, Demo, Shape, T};
use super::{base, local_symbol, public_symbol, validated, DependencyBodyUnit};
use crate::ir::{IrConstructorAccess, IrExpr};
use crate::klib_libraries::{KlibBodyLibrary, KlibDeclarationBodies};
use crate::metadata::id_signature::{
    callable_signature, CallableShape, ClassScope, DeclarationContainer, KlibPublicIdSignature,
    Placement,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrExprKind, KlibIrField,
    KlibIrFunction, KlibIrMember, KlibIrMemberAccess, KlibIrNullability, KlibIrParameter,
    KlibIrStatement, KlibIrType, KlibIrTypeArgument, KlibIrTypeId, KlibIrTypeParameter,
    KlibIrVariance,
};
use crate::metadata::klib_ir::{
    KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::metadata::semantic::KotlinType;
use crate::types::Ty;

const CLASS_SLOT: u32 = 40;
const CONSTRUCTOR_SLOT: u32 = 41;
const FIELD_SLOT: u32 = 42;
const TYPE_PARAMETER_SLOT: u32 = 43;
const THIS_SLOT: u32 = 44;
const PARAMETER_SLOT: u32 = 45;

const MAKE: Shape = ("make", &[T::Int], T::CharSequence);

fn file_local(kind: KlibIrSymbolKind, slot: u32) -> KlibIrSymbol {
    local_symbol(kind, slot)
}

fn private(
    mut declaration: crate::metadata::klib_ir::tree::KlibIrDeclarationBase,
) -> crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
    declaration.flags = 1 << 1;
    declaration
}

fn any_constructor() -> KlibPublicIdSignature {
    let package = ["kotlin".to_owned()];
    let classes = [ClassScope {
        name: "Any",
        type_parameters: Vec::new(),
        expect: false,
    }];
    callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &classes,
            native_interop_library: false,
        },
        &CallableShape {
            name: "<init>",
            contexts: Vec::new(),
            receiver: None,
            params: Vec::new(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .expect("Any.<init> is signable")
}

fn applied_hidden(arena: &mut KlibIrArena, argument: KlibIrTypeId) -> KlibIrTypeId {
    arena.push_type(KlibIrType::Simple {
        classifier: file_local(KlibIrSymbolKind::Class, CLASS_SLOT),
        nullability: KlibIrNullability::NotSpecified,
        arguments: vec![KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty: argument,
        }],
        annotations: Vec::new(),
        abbreviation: None,
    })
}

/// `class Hidden<T>(val value: T) : CharSequence`, private to its file, delegating to `Any`.
fn hidden_class() -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let class_symbol = file_local(KlibIrSymbolKind::Class, CLASS_SLOT);
    let formal = arena.push_type(KlibIrType::Simple {
        classifier: file_local(KlibIrSymbolKind::TypeParameter, TYPE_PARAMETER_SLOT),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let class_ty = applied_hidden(&mut arena, formal);
    let unit = arena.push_type(T::Unit.klib());
    let char_sequence = arena.push_type(T::CharSequence.klib());
    let delegate = arena.push_expr(
        Some(unit),
        KlibIrExprKind::DelegatingConstructorCall(KlibIrMemberAccess {
            symbol: public_symbol(KlibIrSymbolKind::Constructor, any_constructor()),
            arguments: KlibIrArguments::Flat(Vec::new()),
            type_arguments: Vec::new(),
            origin: None,
        }),
    );
    let initialize = arena.push_expr(
        Some(unit),
        KlibIrExprKind::InstanceInitializerCall(class_symbol.clone()),
    );
    let constructor = arena.push_function(KlibIrFunction {
        base: private(base(file_local(
            KlibIrSymbolKind::Constructor,
            CONSTRUCTOR_SLOT,
        ))),
        name: "<init>".to_owned(),
        constructor: true,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: vec![KlibIrParameter {
            base: base(file_local(KlibIrSymbolKind::ValueParameter, PARAMETER_SLOT)),
            name: "value".to_owned(),
            ty: formal,
            vararg_element_type: None,
            default_value: None,
        }],
        return_type: class_ty,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(vec![
            KlibIrStatement::Expression(delegate),
            KlibIrStatement::Expression(initialize),
        ])),
    });
    let mut field = base(file_local(KlibIrSymbolKind::Field, FIELD_SLOT));
    field.flags = (1 << 1) | (1 << 4);
    let mut formal_declaration = base(file_local(
        KlibIrSymbolKind::TypeParameter,
        TYPE_PARAMETER_SLOT,
    ));
    formal_declaration.flags = 4;
    let class = arena.push_class(KlibIrClass {
        base: private(base(class_symbol.clone())),
        name: "Hidden".to_owned(),
        this_receiver: Some(KlibIrParameter {
            base: base(file_local(KlibIrSymbolKind::ValueParameter, THIS_SLOT)),
            name: "<this>".to_owned(),
            ty: class_ty,
            vararg_element_type: None,
            default_value: None,
        }),
        type_parameters: vec![KlibIrTypeParameter {
            base: formal_declaration,
            name: "T".to_owned(),
            supertypes: Vec::new(),
        }],
        supertypes: vec![char_sequence],
        members: vec![
            KlibIrMember::Field(KlibIrField {
                base: field,
                name: "value".to_owned(),
                ty: formal,
                initializer: None,
            }),
            KlibIrMember::Function(constructor),
        ],
        value_class_representation: None,
        sealed_subclasses: Vec::new(),
    });
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Class(class),
    }
}

fn hidden_construction(
    body: &mut super::demo_package::Body,
) -> crate::metadata::klib_ir::tree::KlibIrExprId {
    let argument = body.ty(T::Int);
    let class_ty = applied_hidden(&mut body.arena, argument);
    let value = body.param(0);
    body.arena.push_expr(
        Some(class_ty),
        KlibIrExprKind::ConstructorCall {
            access: KlibIrMemberAccess {
                symbol: file_local(KlibIrSymbolKind::Constructor, CONSTRUCTOR_SLOT),
                arguments: KlibIrArguments::Flat(vec![Some(value)]),
                type_arguments: vec![Some(argument)],
                origin: None,
            },
            constructor_type_arguments: 0,
        },
    )
}

fn hidden_body(body: &mut super::demo_package::Body) -> Vec<KlibIrStatement> {
    let first = hidden_construction(body);
    let second = hidden_construction(body);
    vec![
        KlibIrStatement::Expression(body.coerced(first)),
        body.ret(second),
    ]
}

#[test]
fn a_body_constructs_a_non_public_class_of_its_own_library() {
    let demo = demo_with(&[MAKE], vec![hidden_class()], |_, body| {
        Some(hidden_body(body))
    });
    let mut unit = DependencyBodyUnit::default();
    let function = demo
        .lower(&mut unit, "make", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(demo.lower(&mut unit, "make", &[]).unwrap(), function);
    validated(unit.ir());
    let classifier = super::super::same_library_class::classifier_name(
        "demo",
        &KlibIrSignature::FileLocal {
            file: 0,
            slot: CLASS_SLOT,
        },
    )
    .expect("a file-local class has a classifier");
    let ir = unit.ir();
    assert_eq!(ir.classes.len(), 1);
    assert_eq!(ir.class_id_by_name(classifier), Some(0));
    let class = &ir.classes[0];
    assert_eq!(class.fq_name, classifier);
    assert!(class.has_primary_ctor);
    assert!(!class.explicit_param_stores);
    assert_eq!(class.fields.len(), 1);
    assert_eq!(class.fields[0].name, "value");
    assert!(class.fields[0].ty.is_ty_param());
    assert!(class.ctor_args[0].is_field);
    assert_eq!(class.ctor_args[0].field_index, Some(0));
    assert!(class.ctor_args[0].ty.is_ty_param());
    let constructions = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| match expression {
            IrExpr::New {
                internal,
                external_target,
                ctor_params,
                args,
                ..
            } => Some((
                index as u32,
                *internal,
                external_target.is_none(),
                ctor_params.clone(),
                args.len(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(constructions.len(), 2);
    for (site, internal, local, params, arity) in &constructions {
        assert_eq!(*internal, classifier);
        assert!(*local);
        assert_eq!(params.as_deref(), Some([Ty::Int].as_slice()));
        assert_eq!(*arity, 1);
        let target = ir
            .construction_targets
            .get(site)
            .expect("the construction selects the materialized primary constructor");
        assert_eq!(target.ordinal, 0);
        assert_eq!(target.access, IrConstructorAccess::Private);
    }
}

#[test]
fn a_non_public_class_of_another_library_stays_refused() {
    let (facts, caller) = selected_demo_module("demo", &[MAKE], &["make"], |_, body| {
        let constructed = hidden_construction(body);
        Some(vec![body.ret(constructed)])
    });
    let hidden = KlibIrModuleTrees::from_trees(vec![hidden_class()]).expect("one private class");
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new("demo".to_owned(), ["other".to_owned()], caller),
        KlibBodyLibrary::new("other".to_owned(), Vec::<String>::new(), hidden),
    ])
    .expect("the two libraries have distinct modules");
    let demo = Demo { facts, bodies };
    assert_eq!(
        demo.declined("make", &[]),
        "the KLIB body of `demo.make` (it uses a type of a non-public class)"
    );
}
