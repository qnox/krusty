//! A body may use a non-public class of the library that declares the body. The same file-local
//! identity in another library, even one the producer depends on, stays refused.
//!
//! Stores come from field initializers and anonymous initializers in member order. Two libraries
//! that reuse one file-local signature stay distinct, including after a declined lowering. An
//! interface is a supertype only when this library authenticates it, and that same list is the
//! class's hierarchy.

use super::demo_package::{demo_with, module_trees, selected_demo_module, Demo, Shape, T};
use super::{base, local_symbol, public_symbol, validated, DependencyBodyUnit};
use crate::ir::{IrConst, IrConstructorAccess, IrExpr};
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
    KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol,
    KlibIrSymbolKind,
};
use crate::metadata::semantic::KotlinType;
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

const CLASS_SLOT: u32 = 40;
const CONSTRUCTOR_SLOT: u32 = 41;
const STORED_SLOT: u32 = 42;
const TYPE_PARAMETER_SLOT: u32 = 43;
const THIS_SLOT: u32 = 44;
const PARAMETER_SLOT: u32 = 45;
const MARK_SLOT: u32 = 46;
const INITIALIZER_SLOT: u32 = 47;
const MEMBER_SLOT: u32 = 48;
const MEMBER_THIS_SLOT: u32 = 49;

const MAKE: Shape = ("make", &[T::Int], T::Any);
const EMPTY_MAKE: Shape = ("make", &[], T::Any);

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

fn class_symbol() -> KlibIrSymbol {
    file_local(KlibIrSymbolKind::Class, CLASS_SLOT)
}

fn parameter_symbol() -> KlibIrSymbol {
    file_local(KlibIrSymbolKind::ValueParameter, PARAMETER_SLOT)
}

fn applied_hidden(arena: &mut KlibIrArena, argument: KlibIrTypeId) -> KlibIrTypeId {
    arena.push_type(KlibIrType::Simple {
        classifier: class_symbol(),
        nullability: KlibIrNullability::NotSpecified,
        arguments: vec![KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty: argument,
        }],
        annotations: Vec::new(),
        abbreviation: None,
    })
}

fn plain_hidden(arena: &mut KlibIrArena) -> KlibIrTypeId {
    arena.push_type(KlibIrType::Simple {
        classifier: class_symbol(),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    })
}

fn const_int(arena: &mut KlibIrArena, value: i32) -> crate::metadata::klib_ir::tree::KlibIrExprId {
    let ty = arena.push_type(T::Int.klib());
    arena.push_expr(Some(ty), KlibIrExprKind::Const(KlibIrConstant::Int(value)))
}

fn delegation(arena: &mut KlibIrArena, class_symbol: &KlibIrSymbol) -> Vec<KlibIrStatement> {
    let unit = arena.push_type(T::Unit.klib());
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
    vec![
        KlibIrStatement::Expression(delegate),
        KlibIrStatement::Expression(initialize),
    ]
}

fn field(
    slot: u32,
    name: &str,
    ty: KlibIrTypeId,
    initializer: crate::metadata::klib_ir::tree::KlibIrExprId,
) -> KlibIrField {
    let mut declaration = base(file_local(KlibIrSymbolKind::Field, slot));
    declaration.flags = (1 << 1) | (1 << 4);
    KlibIrField {
        base: declaration,
        name: name.to_owned(),
        ty,
        initializer: Some(initializer),
    }
}

/// `class Hidden<T>(value: T) : Super`, with `stored` initialized from the parameter and `mark`
/// from a later constant. The field spelling is not the parameter spelling.
fn hidden_class(supertype: T) -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let symbol = class_symbol();
    let formal = arena.push_type(KlibIrType::Simple {
        classifier: file_local(KlibIrSymbolKind::TypeParameter, TYPE_PARAMETER_SLOT),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let class_ty = applied_hidden(&mut arena, formal);
    let int = arena.push_type(T::Int.klib());
    let listed = arena.push_type(supertype.klib());
    let parameter = parameter_symbol();
    let stored_init = arena.push_expr(
        Some(formal),
        KlibIrExprKind::GetValue {
            symbol: parameter.clone(),
            origin: None,
        },
    );
    let init_value = const_int(&mut arena, 4);
    let mark_init = const_int(&mut arena, 7);
    let constructor_body = delegation(&mut arena, &symbol);
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
            base: base(parameter),
            name: "value".to_owned(),
            ty: formal,
            vararg_element_type: None,
            default_value: None,
        }],
        return_type: class_ty,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(constructor_body)),
    });
    let mut formal_declaration = base(file_local(
        KlibIrSymbolKind::TypeParameter,
        TYPE_PARAMETER_SLOT,
    ));
    formal_declaration.flags = 4;
    let class = arena.push_class(KlibIrClass {
        base: private(base(symbol.clone())),
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
        supertypes: vec![listed],
        members: vec![
            KlibIrMember::Field(field(STORED_SLOT, "stored", formal, stored_init)),
            KlibIrMember::AnonymousInitializer {
                base: base(file_local(KlibIrSymbolKind::Class, INITIALIZER_SLOT)),
                statements: vec![KlibIrStatement::Expression(init_value)],
            },
            KlibIrMember::Field(field(MARK_SLOT, "mark", int, mark_init)),
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

/// A non-generic class whose only store is the constant `value`, not a constructor parameter.
fn counted_class(value: i32) -> KlibIrDeclarationTree {
    counted_class_with(value, |_, _| Vec::new())
}

/// [`counted_class`] with the further `members` it builds in the class's own arena, given the
/// class type.
fn counted_class_with(
    value: i32,
    members: impl FnOnce(&mut KlibIrArena, KlibIrTypeId) -> Vec<KlibIrMember>,
) -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let symbol = class_symbol();
    let class_ty = plain_hidden(&mut arena);
    let any = arena.push_type(T::Any.klib());
    let int = arena.push_type(T::Int.klib());
    let initializer = const_int(&mut arena, value);
    let constructor_body = delegation(&mut arena, &symbol);
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
        regular_parameters: Vec::new(),
        return_type: class_ty,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(constructor_body)),
    });
    let extra = members(&mut arena, class_ty);
    let class = arena.push_class(KlibIrClass {
        base: private(base(symbol.clone())),
        name: "Hidden".to_owned(),
        this_receiver: Some(KlibIrParameter {
            base: base(file_local(KlibIrSymbolKind::ValueParameter, THIS_SLOT)),
            name: "<this>".to_owned(),
            ty: class_ty,
            vararg_element_type: None,
            default_value: None,
        }),
        type_parameters: Vec::new(),
        supertypes: vec![any],
        members: [
            KlibIrMember::Field(field(STORED_SLOT, "stored", int, initializer)),
            KlibIrMember::Function(constructor),
        ]
        .into_iter()
        .chain(extra)
        .collect(),
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

fn counted_construction(
    body: &mut super::demo_package::Body,
) -> crate::metadata::klib_ir::tree::KlibIrExprId {
    let class_ty = plain_hidden(&mut body.arena);
    body.arena.push_expr(
        Some(class_ty),
        KlibIrExprKind::ConstructorCall {
            access: KlibIrMemberAccess {
                symbol: file_local(KlibIrSymbolKind::Constructor, CONSTRUCTOR_SLOT),
                arguments: KlibIrArguments::Flat(Vec::new()),
                type_arguments: Vec::new(),
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

fn classifier(module: &str) -> crate::types::TypeName {
    super::super::same_library_class::classifier_name(
        module,
        &KlibIrSignature::FileLocal {
            file: 0,
            slot: CLASS_SLOT,
        },
    )
    .expect("a file-local class has a classifier")
}

fn init_statements(ir: &crate::ir::IrFile, class: crate::ir::ClassId) -> Vec<&IrExpr> {
    let init = ir.classes[class as usize].methods[0];
    let body = ir.functions[init as usize]
        .body
        .expect("the initializer has a body");
    let IrExpr::Block { stmts, .. } = &ir.exprs[body as usize] else {
        panic!("the initializer body is a block");
    };
    stmts.iter().map(|id| &ir.exprs[*id as usize]).collect()
}

fn expression(ir: &crate::ir::IrFile, id: u32) -> &IrExpr {
    &ir.exprs[id as usize]
}

#[test]
fn a_body_constructs_a_non_public_class_of_its_own_library() {
    let demo = demo_with(&[MAKE], vec![hidden_class(T::Any)], |_, body| {
        Some(hidden_body(body))
    });
    let mut unit = DependencyBodyUnit::default();
    let function = demo
        .lower(&mut unit, "make", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(demo.lower(&mut unit, "make", &[]).unwrap(), function);
    validated(unit.ir());
    let classifier = classifier("demo");
    let ir = unit.ir();
    assert_eq!(ir.classes.len(), 1);
    assert_eq!(ir.class_id_by_name(classifier), Some(0));
    let class = &ir.classes[0];
    assert_eq!(class.fq_name, classifier);
    assert!(class.has_primary_ctor);
    assert!(class.explicit_param_stores);
    assert_eq!(class.ctor_param_count, 0);
    assert_eq!(class.fields.len(), 2);
    assert_eq!(class.fields[0].name, "stored");
    assert!(class.fields[0].ty.is_ty_param());
    assert_eq!(class.fields[1].name, "mark");
    assert_eq!(class.fields[1].ty, Ty::Int);
    assert!(!class.ctor_args[0].is_field);
    assert_eq!(class.ctor_args[0].field_index, None);
    assert_eq!(class.ctor_args[0].name.as_deref(), Some("value"));
    assert!(class.ctor_args[0].ty.is_ty_param());
    assert_eq!(
        class.supertypes,
        vec![Ty::obj_name(crate::types::wk::any())]
    );
    let statements = init_statements(ir, 0);
    assert_eq!(statements.len(), 3);
    match statements[0] {
        IrExpr::SetField {
            receiver,
            index: 0,
            value,
            ..
        } => {
            assert!(matches!(expression(ir, *receiver), IrExpr::GetValue(0)));
            assert!(matches!(expression(ir, *value), IrExpr::GetValue(1)));
        }
        other => panic!("first initializer: {other:?}"),
    }
    match statements[1] {
        IrExpr::Block {
            value: Some(value), ..
        } => assert!(matches!(
            expression(ir, *value),
            IrExpr::Const(IrConst::Int(4))
        )),
        other => panic!("anonymous initializer: {other:?}"),
    }
    match statements[2] {
        IrExpr::SetField {
            receiver,
            index: 1,
            value,
            ..
        } => {
            assert!(matches!(expression(ir, *receiver), IrExpr::GetValue(0)));
            assert!(matches!(
                expression(ir, *value),
                IrExpr::Const(IrConst::Int(7))
            ));
        }
        other => panic!("second initializer: {other:?}"),
    }
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
    let hidden =
        KlibIrModuleTrees::from_trees(vec![hidden_class(T::Any)]).expect("one private class");
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

#[test]
fn an_unauthenticated_interface_is_refused_before_admission() {
    let demo = demo_with(&[MAKE], vec![hidden_class(T::CharSequence)], |_, body| {
        Some(hidden_body(body))
    });
    assert_eq!(
        demo.declined("make", &[]),
        "the KLIB body of `demo.make` (it declares a supertype this library does not authenticate)"
    );
}

fn stored_constant(ir: &crate::ir::IrFile, module: &str) -> i32 {
    let class = ir
        .class_id_by_name(classifier(module))
        .expect("the library's class");
    let statements = init_statements(ir, class);
    let IrExpr::SetField {
        value, index: 0, ..
    } = statements[0]
    else {
        panic!("the constant is the class's only store");
    };
    let IrExpr::Const(IrConst::Int(value)) = expression(ir, *value) else {
        panic!("the store lowers the field initializer");
    };
    *value
}

fn lower_named(
    unit: &mut DependencyBodyUnit,
    facts: &crate::fx_hash::HashMap<&'static str, crate::backend::BackendCallableFact>,
    bodies: &KlibDeclarationBodies,
    name: &str,
) -> Result<crate::ir::FunId, crate::klib_lowering::KlibBodyDecline> {
    let callable = facts[name]
        .klib_body_callable()
        .expect("a demo function is signed");
    let callees = super::super::KlibCalleeFacts::new([callable]);
    unit.lower_function(callable, bodies, &callees)
}

fn two_libraries(
    beta: &[Shape],
    beta_body: impl Fn(&str, &mut super::demo_package::Body) -> Option<Vec<KlibIrStatement>>,
) -> (
    crate::fx_hash::HashMap<&'static str, crate::backend::BackendCallableFact>,
    crate::fx_hash::HashMap<&'static str, crate::backend::BackendCallableFact>,
    KlibDeclarationBodies,
) {
    let (alpha_facts, alpha) =
        module_trees("alpha", &[EMPTY_MAKE], vec![counted_class(3)], |_, body| {
            let constructed = counted_construction(body);
            Some(vec![body.ret(constructed)])
        });
    let (beta_facts, beta_trees) = module_trees("beta", beta, vec![counted_class(9)], beta_body);
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new("alpha".to_owned(), Vec::<String>::new(), alpha),
        KlibBodyLibrary::new("beta".to_owned(), Vec::<String>::new(), beta_trees),
    ])
    .expect("the two libraries have distinct modules");
    (alpha_facts, beta_facts, bodies)
}

#[test]
fn equal_file_local_signatures_stay_distinct_in_either_order() {
    let (alpha_facts, beta_facts, bodies) = two_libraries(&[EMPTY_MAKE], |_, body| {
        let constructed = counted_construction(body);
        Some(vec![body.ret(constructed)])
    });
    for (first, second) in [("alpha", "beta"), ("beta", "alpha")] {
        let mut unit = DependencyBodyUnit::default();
        let (first_facts, second_facts) = if first == "alpha" {
            (&alpha_facts, &beta_facts)
        } else {
            (&beta_facts, &alpha_facts)
        };
        lower_named(&mut unit, first_facts, &bodies, "make")
            .unwrap_or_else(|decline| panic!("{first}: {decline}"));
        lower_named(&mut unit, second_facts, &bodies, "make")
            .unwrap_or_else(|decline| panic!("{second}: {decline}"));
        validated(unit.ir());
        assert_eq!(stored_constant(unit.ir(), "alpha"), 3);
        assert_eq!(stored_constant(unit.ir(), "beta"), 9);
        assert_ne!(classifier("alpha"), classifier("beta"));
    }
}

#[test]
fn a_declined_library_does_not_keep_another_librarys_class() {
    let (alpha_facts, beta_facts, bodies) = two_libraries(
        &[("bad", &[], T::Any), ("good", &[], T::Any)],
        |name, body| match name {
            "good" => {
                let constructed = counted_construction(body);
                Some(vec![body.ret(constructed)])
            }
            "bad" => {
                let constructed = counted_construction(body);
                let dynamic = body.arena.push_type(KlibIrType::Dynamic {
                    annotations: Vec::new(),
                });
                let broken = body
                    .arena
                    .push_expr(Some(dynamic), KlibIrExprKind::Const(KlibIrConstant::Int(1)));
                let discarded = body.coerced(constructed);
                Some(vec![
                    KlibIrStatement::Expression(discarded),
                    body.ret(broken),
                ])
            }
            _ => None,
        },
    );
    let mut unit = DependencyBodyUnit::default();
    lower_named(&mut unit, &alpha_facts, &bodies, "make")
        .unwrap_or_else(|decline| panic!("{decline}"));
    let functions = unit.ir().functions.len();
    let decline = lower_named(&mut unit, &beta_facts, &bodies, "bad")
        .expect_err("the dynamic type declines the lowering");
    assert!(
        decline.to_string().contains("the dynamic type"),
        "{decline}"
    );
    assert_eq!(unit.ir().classes.len(), 1);
    assert_eq!(unit.ir().functions.len(), functions);
    assert!(unit.ir().class_id_by_name(classifier("beta")).is_none());
    assert_eq!(stored_constant(unit.ir(), "alpha"), 3);
    lower_named(&mut unit, &beta_facts, &bodies, "good")
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    assert_eq!(stored_constant(unit.ir(), "alpha"), 3);
    assert_eq!(stored_constant(unit.ir(), "beta"), 9);
}

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_same_library_class/Library.kt");
const INTERFACE_DISPATCH: &str =
    include_str!("../../../tests/box_regressions/sameLibraryInterfaceDispatch.kt");
const APPLIED_INTERFACE: &str = "\
package fixture
interface View<T> {
    fun get(): T
}
private class Hidden(private val value: String) : View<CharSequence> {
    override fun get(): String = value
    override fun toString(): String = \"value=$value\"
}
fun make(value: String): View<CharSequence> = Hidden(value)
";

#[test]
fn official_klib_initializes_a_private_class_and_calls_its_members() {
    let Some(root) = super::distribution_root() else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "krusty-klib-same-library-class-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create producer directory");
    let source = directory.join("Library.kt");
    std::fs::write(&source, LIBRARY).unwrap();
    let library = directory.join("hidden");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = crate::klib_libraries::KlibLibraries::open(&[
        crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            crate::klib_libraries::KlibStdlibSemantics::Native,
        ),
        crate::klib_libraries::KlibLibraryInput::dependency(library),
    ])
    .expect("read the serialized library");
    let mut unit = DependencyBodyUnit::default();
    let externals = marker_externals(&libraries);
    for name in ["make", "marked", "asMarker"] {
        let symbols = libraries.symbols(
            crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("fixture")),
            name,
        );
        let [function] = symbols.callables.functions() else {
            panic!("one producer declaration {name}")
        };
        let selected = libraries
            .external_callable(function.callable.external_identity.unwrap())
            .unwrap();
        let callable = selected.klib_body_callable().unwrap();
        let callees = super::super::KlibCalleeFacts::new([callable]);
        unit.lower_function_with_declarations(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
            &externals,
            &libraries,
        )
        .unwrap_or_else(|decline| panic!("{name}: {decline}"));
    }
    validated(unit.ir());
    let ir = unit.ir();
    let class_index = ir
        .classes
        .iter()
        .position(|class| class.fq_name.render().contains("klib-local"))
        .expect("the private class is materialized");
    let class = &ir.classes[class_index];
    assert!(class.explicit_param_stores);
    assert!(class.ctor_args.iter().all(|argument| !argument.is_field));
    let supertypes = class
        .supertypes
        .iter()
        .map(|ty| ty.kotlin_class_internal().unwrap().render())
        .collect::<Vec<_>>();
    assert!(
        supertypes.iter().any(|name| name.contains("Marker")),
        "{supertypes:?}"
    );
    assert!(class
        .interfaces
        .iter()
        .any(|name| name.render().contains("Marker")));
    let methods = class
        .methods
        .iter()
        .map(|function| ir.functions[*function as usize].name.clone())
        .collect::<Vec<_>>();
    assert!(methods.iter().any(|name| name == "member"), "{methods:?}");
    assert!(methods.iter().any(|name| name == "mark"), "{methods:?}");
    let init = class.methods[0];
    assert_eq!(ir.functions[init as usize].name, "<init>");
    let statements = init_statements(ir, u32::try_from(class_index).unwrap());
    let stores = statements
        .iter()
        .filter_map(|statement| match statement {
            IrExpr::SetField { index, value, .. } => Some((*index, *value)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(stores.len() >= 2, "{statements:?}");
    assert!(matches!(expression(ir, stores[0].1), IrExpr::GetValue(_)));
    assert!(
        matches!(expression(ir, stores[1].1), IrExpr::Call { .. }),
        "the later field initializer calls a member: {:?}",
        expression(ir, stores[1].1)
    );
    let calls = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call { callee, .. } => match callee.as_ref() {
                crate::ir::Callee::Local(function) => {
                    Some(ir.functions[*function as usize].name.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(calls.iter().any(|name| name == "member"), "{calls:?}");
    assert!(calls.iter().any(|name| name == "mark"), "{calls:?}");
    let edges = ir
        .function_overrides
        .get(&class.fq_name_id())
        .expect("the private class publishes its override graph");
    assert!(
        edges
            .iter()
            .any(|edge| edge.overridden_is_interface && edge.name == "mark"),
        "{edges:?}"
    );
}

#[test]
fn interface_typed_call_targets_the_authenticated_override() {
    let Some(root) = super::distribution_root() else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "krusty-klib-interface-dispatch-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create producer directory");
    let source = directory.join("InterfaceDispatch.kt");
    std::fs::write(&source, INTERFACE_DISPATCH).unwrap();
    let library = directory.join("dispatch");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = crate::klib_libraries::KlibLibraries::open(&[
        crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            crate::klib_libraries::KlibStdlibSemantics::Native,
        ),
        crate::klib_libraries::KlibLibraryInput::dependency(library),
    ])
    .expect("read the serialized library");
    let mark = marker_mark(&libraries);
    let mut unit = DependencyBodyUnit::default();
    let symbols = libraries.symbols(
        crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("fixture")),
        "through",
    );
    let [function] = symbols.callables.functions() else {
        panic!("one producer declaration through")
    };
    let selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let callable = selected.klib_body_callable().unwrap();
    let callees = super::super::KlibCalleeFacts::new([callable]);
    let externals = [(
        mark.identity.clone(),
        super::super::EmittedDependencyCallable::new(mark.id, None, None),
    )]
    .into_iter()
    .collect();
    unit.lower_function_with_declarations(
        callable,
        libraries.declaration_bodies(),
        &callees,
        libraries.intrinsic_facts(),
        &externals,
        &libraries,
    )
    .unwrap_or_else(|decline| panic!("through: {decline}"));
    validated(unit.ir());
    let ir = unit.ir();
    let targets = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call { callee, .. } => match callee.as_ref() {
                crate::ir::Callee::External { target, .. } => Some(*target),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, vec![mark.id], "{targets:?}");
    let edge = ir
        .function_overrides
        .values()
        .flatten()
        .find(|edge| {
            edge.overridden == crate::fir::ResolvedFunctionOverrideTarget::External(mark.id)
        })
        .expect("the private class publishes Marker.mark");
    assert!(edge.overridden_is_interface);
    assert!(edge.implementation_function.is_some());
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn returned_interface_publishes_the_override_without_calling_it() {
    let Some((directory, libraries)) = produce_library("returned-interface", INTERFACE_DISPATCH)
    else {
        return;
    };
    let mark = declared_member(&libraries, "Marker", "mark");
    let mut unit = DependencyBodyUnit::default();
    lower_fixture(
        &mut unit,
        &libraries,
        "make",
        &Default::default(),
        &libraries,
    );
    validated(unit.ir());
    let edge = override_edge(unit.ir(), mark.id);
    assert!(edge.overridden_is_interface);
    assert!(edge.implementation_function.is_some());
    assert!(
        unit.ir().exprs.iter().all(|expression| match expression {
            IrExpr::Call { callee, .. } => !matches!(
                callee.as_ref(),
                crate::ir::Callee::External { target, .. } if *target == mark.id
            ),
            _ => true,
        }),
        "make constructs Hidden without calling Marker.mark"
    );
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn applied_interface_signature_substitutes_the_supertype() {
    let Some((directory, libraries)) = produce_library("applied-interface", APPLIED_INTERFACE)
    else {
        return;
    };
    let get = declared_member(&libraries, "View", "get");
    let mut unit = DependencyBodyUnit::default();
    lower_fixture(
        &mut unit,
        &libraries,
        "make",
        &Default::default(),
        &libraries,
    );
    validated(unit.ir());
    let edge = override_edge(unit.ir(), get.id);
    assert!(
        edge.declared_result.is_ty_param(),
        "{:?}",
        edge.declared_result
    );
    assert_eq!(type_name_of(edge.applied_result), "kotlin/CharSequence");
    assert_eq!(type_name_of(edge.implementation_result), "kotlin/String");
    assert_ne!(edge.applied_result, edge.implementation_result);
    assert_ne!(edge.declared_result, edge.applied_result);
    assert!(edge.applied_parameters.is_empty());
    assert!(edge.declared_parameters.is_empty());
    let any_to_string = unit
        .ir()
        .function_overrides
        .values()
        .flatten()
        .find(|edge| edge.overridden_owner == crate::types::wk::any() && edge.name == "toString")
        .expect("Hidden publishes its Any.toString override");
    let any_symbols = libraries.symbols(
        crate::symbol_source::SymbolNamespace::Classifier(crate::types::wk::any()),
        "toString",
    );
    let [any_declaration] = any_symbols.callables.functions() else {
        panic!("one kotlin.Any.toString declaration")
    };
    let any_to_string_id = any_declaration
        .callable
        .external_identity
        .expect("kotlin.Any.toString has a provider identity");
    assert_eq!(
        any_to_string.overridden,
        crate::fir::ResolvedFunctionOverrideTarget::External(any_to_string_id)
    );
    assert!(!any_to_string.overridden_is_interface);
    assert!(any_to_string.implementation_function.is_some());
    assert_eq!(
        any_to_string.overridden_semantic_role,
        Some(crate::types::SemanticCallRole::KotlinAnyToString)
    );
    let _ = std::fs::remove_dir_all(&directory);
}

struct MarkIdentity {
    id: crate::fir::ExternalCallableId,
    identity: crate::libraries::KlibDependencyCallable,
}

fn marker_mark(libraries: &crate::klib_libraries::KlibLibraries) -> MarkIdentity {
    declared_member(libraries, "Marker", "mark")
}

fn declared_member(
    libraries: &crate::klib_libraries::KlibLibraries,
    owner: &str,
    name: &str,
) -> MarkIdentity {
    let symbols = libraries.symbols(
        crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("fixture")),
        owner,
    );
    let classifier = symbols
        .classifier
        .as_ref()
        .unwrap_or_else(|| panic!("fixture.{owner}"));
    let callables = classifier
        .declared_callables
        .get(name)
        .unwrap_or_else(|| panic!("fixture.{owner}.{name}"));
    let [function] = callables.functions() else {
        panic!(
            "one fixture.{owner}.{name}, found {}",
            callables.functions().len()
        )
    };
    let id = function
        .callable
        .external_identity
        .unwrap_or_else(|| panic!("fixture.{owner}.{name} identity"));
    let selected = libraries.external_callable(id).unwrap();
    let identity = selected
        .dependency_callable
        .clone()
        .unwrap_or_else(|| panic!("fixture.{owner}.{name} dependency identity"));
    MarkIdentity { id, identity }
}

type ProducedExternals = crate::fx_hash::HashMap<
    crate::libraries::KlibDependencyCallable,
    super::super::EmittedDependencyCallable,
>;

fn produce_library(
    name: &str,
    source_text: &str,
) -> Option<(std::path::PathBuf, crate::klib_libraries::KlibLibraries)> {
    let root = super::distribution_root()?;
    let directory = std::env::temp_dir().join(format!("krusty-klib-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create producer directory");
    let source = directory.join(format!("{name}.kt"));
    std::fs::write(&source, source_text).unwrap();
    let library = directory.join(name);
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = crate::klib_libraries::KlibLibraries::open(&[
        crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            crate::klib_libraries::KlibStdlibSemantics::Native,
        ),
        crate::klib_libraries::KlibLibraryInput::dependency(library),
    ])
    .expect("read the serialized library");
    Some((directory, libraries))
}

fn lower_fixture(
    unit: &mut DependencyBodyUnit,
    libraries: &crate::klib_libraries::KlibLibraries,
    name: &str,
    externals: &ProducedExternals,
    declarations: &dyn SymbolSource,
) {
    let symbols = libraries.symbols(
        crate::symbol_source::SymbolNamespace::Package(crate::types::type_name("fixture")),
        name,
    );
    let [function] = symbols.callables.functions() else {
        panic!("one producer declaration {name}")
    };
    let selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let callable = selected.klib_body_callable().unwrap();
    let callees = super::super::KlibCalleeFacts::new([callable]);
    unit.lower_function_with_declarations(
        callable,
        libraries.declaration_bodies(),
        &callees,
        libraries.intrinsic_facts(),
        externals,
        declarations,
    )
    .unwrap_or_else(|decline| panic!("{name}: {decline}"));
}

fn override_edge(
    ir: &crate::ir::IrFile,
    id: crate::fir::ExternalCallableId,
) -> &crate::ir::IrFunctionOverride {
    ir.function_overrides
        .values()
        .flatten()
        .find(|edge| edge.overridden == crate::fir::ResolvedFunctionOverrideTarget::External(id))
        .unwrap_or_else(|| panic!("missing override {id:?}"))
}

fn type_name_of(ty: Ty) -> String {
    ty.kotlin_class_internal()
        .map(|name| name.render())
        .unwrap_or_else(|| format!("{ty:?}"))
}

fn marker_externals(
    libraries: &crate::klib_libraries::KlibLibraries,
) -> crate::fx_hash::HashMap<
    crate::libraries::KlibDependencyCallable,
    super::super::EmittedDependencyCallable,
> {
    let mark = marker_mark(libraries);
    [(
        mark.identity,
        super::super::EmittedDependencyCallable::new(mark.id, None, None),
    )]
    .into_iter()
    .collect()
}

/// A non-public class is its own declaration tree, apart from the body constructing it. The
/// callees of its members are emitted with that body, as dispatch can run any member.
#[test]
fn a_constructed_class_of_its_own_tree_contributes_its_member_callees() {
    const HELPER: Shape = ("helper", &[], T::Int);
    let build = |name: &str, body: &mut super::demo_package::Body| match name {
        "make" => {
            let constructed = counted_construction(body);
            Some(vec![body.ret(constructed)])
        }
        "helper" => {
            let value = body.int(1);
            Some(vec![body.ret(value)])
        }
        _ => None,
    };
    let shapes = [EMPTY_MAKE, HELPER];
    let signed = demo_with(&shapes, vec![counted_class(3)], build);
    let Some(crate::libraries::KlibDeclarationSignature::Public(helper)) =
        signed.facts["helper"].declaration_signature.clone()
    else {
        panic!("a top-level KLIB function freezes its public signature")
    };
    let class = helper_calling_class(helper);
    let demo = demo_with(&shapes, vec![class], build);
    let make = demo.callable("make").identity().clone();
    let helper = demo.callable("helper").identity().clone();
    assert_eq!(
        demo.bodies
            .emitted_body_callables(&make, crate::klib_lowering::EMITTED_CALL_FORMS),
        vec![helper]
    );
}

/// A file-local symbol is a slot of the signature tables of the trees that serialize the body.
/// A body of another component (a prepared-inline copy) that names the same file and slot as an
/// ordinary private class does not reach that class, so its members contribute nothing.
#[test]
fn a_colliding_file_local_symbol_of_another_component_contributes_nothing() {
    const HELPER: Shape = ("helper", &[], T::Int);
    let build = |name: &str, body: &mut super::demo_package::Body| match name {
        "make" => {
            let constructed = counted_construction(body);
            Some(vec![body.ret(constructed)])
        }
        "helper" => {
            let value = body.int(1);
            Some(vec![body.ret(value)])
        }
        _ => None,
    };
    let shapes = [EMPTY_MAKE, HELPER];
    let signed = demo_with(&shapes, vec![counted_class(3)], build);
    let Some(crate::libraries::KlibDeclarationSignature::Public(helper)) =
        signed.facts["helper"].declaration_signature.clone()
    else {
        panic!("a top-level KLIB function freezes its public signature")
    };
    let ordinary = demo_with(&shapes, vec![helper_calling_class(helper)], build);
    let make = ordinary.callable("make").identity().clone();
    assert_eq!(
        ordinary
            .bodies
            .emitted_body_callables(&make, crate::klib_lowering::EMITTED_CALL_FORMS),
        vec![ordinary.callable("helper").identity().clone()]
    );
    // The other component serializes the same constructing body but no class at those
    // coordinates.
    let (_, prepared) = module_trees("demo", &shapes, Vec::new(), build);
    let prepared = KlibDeclarationBodies::from_libraries(vec![("demo".to_owned(), prepared)])
        .expect("one library");
    assert_eq!(
        prepared.emitted_body_callables(&make, crate::klib_lowering::EMITTED_CALL_FORMS),
        Vec::new()
    );
}

/// The prepared-inline component and the ordinary IR number their file-local symbols with
/// different signature tables, and the file index of a prepared table can equal an ordinary
/// file's. The two are never decoded into one set of trees (`read_declaration_trees_from` takes
/// one component per archive), and a library set refuses a second library for one module (and a
/// second owner of a declaration), so a file-local
/// symbol is resolved against the single table kind that numbered it.
#[test]
fn one_module_cannot_hold_the_ordinary_and_the_prepared_component_together() {
    let build = |name: &str, body: &mut super::demo_package::Body| match name {
        "make" => {
            let constructed = counted_construction(body);
            Some(vec![body.ret(constructed)])
        }
        _ => None,
    };
    let shapes = [EMPTY_MAKE];
    let (_, ordinary) = module_trees("demo", &shapes, vec![counted_class(3)], build);
    let (_, prepared) = module_trees("demo", &shapes, Vec::new(), build);
    let error = match KlibDeclarationBodies::from_libraries(vec![
        ("demo".to_owned(), ordinary),
        ("demo".to_owned(), prepared),
    ]) {
        Ok(_) => panic!("two components of one module are refused"),
        Err(error) => error,
    };
    assert_eq!(error.libraries(), [0, 1]);
}

/// A private class whose member `read` calls the public function `helper`.
fn helper_calling_class(helper: KlibPublicIdSignature) -> KlibIrDeclarationTree {
    counted_class_with(3, |arena, class_ty| {
        let int = arena.push_type(T::Int.klib());
        let call = arena.push_expr(
            Some(int),
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: public_symbol(KlibIrSymbolKind::Function, helper),
                    arguments: KlibIrArguments::Flat(Vec::new()),
                    type_arguments: Vec::new(),
                    origin: None,
                },
                super_qualifier: None,
            },
        );
        let member = arena.push_function(KlibIrFunction {
            base: private(base(file_local(KlibIrSymbolKind::Function, MEMBER_SLOT))),
            name: "read".to_owned(),
            constructor: false,
            type_parameters: Vec::new(),
            dispatch_receiver: Some(KlibIrParameter {
                base: base(file_local(
                    KlibIrSymbolKind::ValueParameter,
                    MEMBER_THIS_SLOT,
                )),
                name: "<this>".to_owned(),
                ty: class_ty,
                vararg_element_type: None,
                default_value: None,
            }),
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type: int,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(call)])),
        });
        vec![KlibIrMember::Function(member)]
    })
}
