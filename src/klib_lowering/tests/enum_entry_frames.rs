//! A dependency enum's constants pass its constructor arbitrary expressions, as kotlinc's class
//! initializer evaluates them: the unit lowers each constant's arguments in the enum's own
//! initialization frame, in declaration order, and the shared enum realization evaluates them.

use super::demo_package::{demo_with, operator, T};
use super::{base, local_symbol, public_symbol, DependencyBodyUnit};
use crate::ir::{IrConst, IrExpr};
use crate::libraries::{KlibBodyClassContract, KlibBodyClassParameters, TypeKind};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrEnumEntry, KlibIrExprId,
    KlibIrExprKind, KlibIrField, KlibIrFunction, KlibIrMember, KlibIrMemberAccess,
    KlibIrNullability, KlibIrParameter, KlibIrStatement, KlibIrType, KlibIrTypeArgument,
    KlibIrTypeId, KlibIrTypeParameter, KlibIrVariable, KlibIrVariance,
};
use crate::metadata::klib_ir::{
    KlibIrConstant, KlibIrDeclarationTree, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::metadata::semantic::KotlinModality;
use crate::types::{type_name, Ty, TypeName};

/// Class flags whose kind bits name an enum class.
const ENUM_CLASS: u64 = 2 << 6;
/// Declaration flags of a private declaration.
const PRIVATE: u64 = 1 << 1;

fn mode() -> TypeName {
    type_name("demo/Mode")
}

fn class_type(
    arena: &mut KlibIrArena,
    classifier: TypeName,
    arguments: Vec<KlibIrTypeId>,
) -> KlibIrTypeId {
    arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(
            KlibIrSymbolKind::Class,
            KlibPublicIdSignature::classifier(classifier),
        ),
        nullability: KlibIrNullability::NotSpecified,
        arguments: arguments
            .into_iter()
            .map(|ty| KlibIrTypeArgument::Type {
                variance: KlibIrVariance::Invariant,
                ty,
            })
            .collect(),
        annotations: Vec::new(),
        abbreviation: None,
    })
}

fn kotlin_enum_constructor() -> KlibPublicIdSignature {
    operator(
        &["kotlin"],
        Some("Enum"),
        "<init>",
        &[T::String.kotlin(), T::Int.kotlin()],
    )
    .0
}

fn mode_constructor() -> KlibPublicIdSignature {
    operator(&["demo"], Some("Mode"), "<init>", &[T::Int.kotlin()]).0
}

fn entry_signature(name: &str) -> KlibPublicIdSignature {
    KlibPublicIdSignature::classifier(type_name(&format!("demo/Mode.{name}")))
}

fn constructor(
    symbol: KlibIrSymbol,
    flags: u64,
    parameters: Vec<KlibIrParameter>,
    result: KlibIrTypeId,
    body: Option<Vec<KlibIrStatement>>,
) -> KlibIrFunction {
    KlibIrFunction {
        base: crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
            flags,
            ..base(symbol)
        },
        name: "<init>".into(),
        constructor: true,
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

fn parameter(slot: u32, name: &str, ty: KlibIrTypeId) -> KlibIrParameter {
    KlibIrParameter {
        base: base(local_symbol(KlibIrSymbolKind::ValueParameter, slot)),
        name: name.into(),
        ty,
        vararg_element_type: None,
        default_value: None,
    }
}

/// `kotlin.Enum<E>`'s constructor, which the target lays out.
fn kotlin_enum() -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let formal = arena.push_type(KlibIrType::Simple {
        classifier: local_symbol(KlibIrSymbolKind::TypeParameter, 1),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let own = class_type(&mut arena, crate::types::wk::kotlin_enum(), vec![formal]);
    let string = arena.push_type(T::String.klib());
    let int = arena.push_type(T::Int.klib());
    let header = arena.push_function(constructor(
        public_symbol(KlibIrSymbolKind::Constructor, kotlin_enum_constructor()),
        6,
        vec![parameter(2, "name", string), parameter(3, "ordinal", int)],
        own,
        None,
    ));
    let class = arena.push_class(KlibIrClass {
        base: base(public_symbol(
            KlibIrSymbolKind::Class,
            KlibPublicIdSignature::classifier(crate::types::wk::kotlin_enum()),
        )),
        name: "Enum".into(),
        this_receiver: None,
        type_parameters: vec![KlibIrTypeParameter {
            base: base(local_symbol(KlibIrSymbolKind::TypeParameter, 1)),
            name: "E".into(),
            supertypes: Vec::new(),
        }],
        supertypes: Vec::new(),
        members: vec![KlibIrMember::Function(header)],
        value_class_representation: None,
        sealed_subclasses: Vec::new(),
    });
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Class(class),
    }
}

/// `enum class Mode(val code: Int) { FIRST(run { val seed = 40; seed }), SECOND(7) }`: the first
/// constant's argument is a block declaring a local, not a constant.
fn mode_class() -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let own = class_type(&mut arena, mode(), Vec::new());
    let int = arena.push_type(T::Int.klib());
    let unit = arena.push_type(T::Unit.klib());
    let applied_enum = class_type(&mut arena, crate::types::wk::kotlin_enum(), vec![own]);
    let class_symbol = public_symbol(
        KlibIrSymbolKind::Class,
        KlibPublicIdSignature::classifier(mode()),
    );
    let code = parameter(4, "code", int);
    let stored = arena.push_expr(
        Some(int),
        KlibIrExprKind::GetValue {
            symbol: code.base.symbol.clone(),
            origin: None,
        },
    );
    let delegate = arena.push_expr(
        Some(unit),
        KlibIrExprKind::EnumConstructorCall(KlibIrMemberAccess {
            symbol: public_symbol(KlibIrSymbolKind::Constructor, kotlin_enum_constructor()),
            arguments: KlibIrArguments::Flat(vec![None, None]),
            type_arguments: vec![Some(own)],
            origin: None,
        }),
    );
    let initialize = arena.push_expr(
        Some(unit),
        KlibIrExprKind::InstanceInitializerCall(class_symbol.clone()),
    );
    let primary = arena.push_function(constructor(
        public_symbol(KlibIrSymbolKind::Constructor, mode_constructor()),
        PRIVATE,
        vec![code],
        own,
        Some(vec![
            KlibIrStatement::Expression(delegate),
            KlibIrStatement::Expression(initialize),
        ]),
    ));
    // `run { val seed = 40; seed }`, as a block the entry's initializer evaluates.
    let forty = arena.push_expr(Some(int), KlibIrExprKind::Const(KlibIrConstant::Int(40)));
    let seed_symbol = local_symbol(KlibIrSymbolKind::Variable, 5);
    let seed = arena.push_variable(KlibIrVariable {
        base: base(seed_symbol.clone()),
        name: "seed".into(),
        ty: int,
        initializer: Some(forty),
    });
    let read = arena.push_expr(
        Some(int),
        KlibIrExprKind::GetValue {
            symbol: seed_symbol,
            origin: None,
        },
    );
    let computed = arena.push_expr(
        Some(int),
        KlibIrExprKind::Block {
            statements: vec![
                KlibIrStatement::Variable(seed),
                KlibIrStatement::Expression(read),
            ],
            origin: None,
        },
    );
    let seven = arena.push_expr(Some(int), KlibIrExprKind::Const(KlibIrConstant::Int(7)));
    let entry = |arena: &mut KlibIrArena, name: &str, argument: KlibIrExprId| {
        let call = arena.push_expr(
            Some(own),
            KlibIrExprKind::EnumConstructorCall(KlibIrMemberAccess {
                symbol: public_symbol(KlibIrSymbolKind::Constructor, mode_constructor()),
                arguments: KlibIrArguments::Flat(vec![Some(argument)]),
                type_arguments: Vec::new(),
                origin: None,
            }),
        );
        KlibIrMember::EnumEntry(KlibIrEnumEntry {
            base: base(public_symbol(
                KlibIrSymbolKind::EnumEntry,
                entry_signature(name),
            )),
            name: name.into(),
            initializer: Some(call),
            class: None,
        })
    };
    let first = entry(&mut arena, "FIRST", computed);
    let second = entry(&mut arena, "SECOND", seven);
    let class = arena.push_class(KlibIrClass {
        base: crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
            flags: ENUM_CLASS,
            ..base(class_symbol)
        },
        name: "Mode".into(),
        this_receiver: Some(parameter(6, "<this>", own)),
        type_parameters: Vec::new(),
        supertypes: vec![applied_enum],
        members: vec![
            KlibIrMember::Function(primary),
            KlibIrMember::Field(KlibIrField {
                base: crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
                    flags: PRIVATE | (1 << 4),
                    ..base(local_symbol(KlibIrSymbolKind::Field, 7))
                },
                name: "code".into(),
                ty: int,
                initializer: Some(stored),
            }),
            first,
            second,
        ],
        value_class_representation: None,
        sealed_subclasses: Vec::new(),
    });
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Class(class),
    }
}

fn mode_contract() -> KlibBodyClassContract {
    KlibBodyClassContract {
        module: "demo".into(),
        signature: KlibPublicIdSignature::classifier(mode()),
        classifier: mode(),
        parameters: KlibBodyClassParameters {
            formals: Box::default(),
            bounds: Box::default(),
            variances: Box::default(),
            own_count: 0,
        },
        kind: TypeKind::Enum,
        modality: KotlinModality::Final,
        is_value: false,
        is_inner: false,
        is_expect: false,
        primary_constructor: Some(mode_constructor()),
    }
}

fn represents(classifier: TypeName) -> bool {
    classifier == crate::types::wk::kotlin_enum() || classifier == crate::types::wk::any()
}

#[test]
fn an_enum_constant_s_arguments_are_lowered_in_the_enum_s_initialization_frame() {
    // `fun touch() { Mode.FIRST }` reads a constant, which materializes the enum.
    let demo = demo_with(
        &[("touch", &[], T::Unit)],
        vec![kotlin_enum(), mode_class()],
        |_, body| {
            let own = class_type(&mut body.arena, mode(), Vec::new());
            let read = body.arena.push_expr(
                Some(own),
                KlibIrExprKind::GetEnumValue(public_symbol(
                    KlibIrSymbolKind::EnumEntry,
                    entry_signature("FIRST"),
                )),
            );
            Some(vec![KlibIrStatement::Expression(body.coerced(read))])
        },
    );
    let facts = demo
        .callees(&[])
        .with_target_classifiers(&represents)
        .with_read_classes(vec![mode_contract()]);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function(demo.callable("touch"), &demo.bodies, &facts)
        .unwrap_or_else(|decline| panic!("touch: {decline}"));
    let ir = unit.ir();
    let class = &ir.classes[ir.class_id_by_name(mode()).expect("materialized") as usize];
    assert!(class.is_enum);
    assert_eq!(class.superclass, crate::types::wk::kotlin_enum());
    let entries = class
        .enum_entries
        .iter()
        .map(|entry| {
            (
                entry.name.as_str(),
                entry.constructor_parameter_types.clone(),
                entry.args.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        [("FIRST", vec![Ty::Int], 1), ("SECOND", vec![Ty::Int], 1)]
    );
    // The first constant's argument stays the block that declares its local; the second is the
    // constant it passes.
    let IrExpr::Block {
        stmts,
        value: Some(value),
        ..
    } = ir.expr(class.enum_entries[0].args[0])
    else {
        panic!(
            "FIRST's argument: {:?}",
            ir.expr(class.enum_entries[0].args[0])
        );
    };
    assert_eq!(stmts.len(), 1);
    assert!(matches!(ir.expr(stmts[0]), IrExpr::Variable { .. }));
    assert!(matches!(ir.expr(*value), IrExpr::GetValue(_)));
    assert!(matches!(
        ir.expr(class.enum_entries[1].args[0]),
        IrExpr::Const(IrConst::Int(7))
    ));

    // The shared realization evaluates the lowered arguments as it does a source enum's.
    let mut realized = unit.into_ir();
    crate::backend::enums::realize(
        &mut realized,
        crate::backend::enums::Contract::KotlinNative,
        None,
        &mut crate::backend::DependencyExportSignatures::default(),
    )
    .expect("the Native contract builds the constants");
    assert!(!realized
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::EnumEntry { .. })));
}
