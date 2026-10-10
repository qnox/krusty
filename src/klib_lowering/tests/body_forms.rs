//! Lowering of the KLIB body forms beyond a single relation: calls of dependency functions, the
//! equality and negation built-ins, local variables, no-op implicit casts and multi-branch
//! `when`s. Each lowered body is compared with checked FIR lowering of the equivalent source, on
//! hand-built trees and on the Kotlin/Native stdlib bodies that use the form.

use std::collections::HashMap;

use super::super::*;
use super::{
    base, distribution_root, frozen_function, kotlin_class, local_symbol, lowered_body,
    public_symbol, render, stdlib, validated,
};
use crate::backend::BackendCallableFact;
use crate::ir::FunId;
use crate::klib_libraries::{KlibDeclarationBodies, KlibLibraries};
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::{
    callable_signature, metadata_class_signature, CallableShape, ClassScope, DeclarationContainer,
    KlibPublicIdSignature, MetadataClass, MetadataContainer, Placement,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrBranch, KlibIrExprId, KlibIrExprKind,
    KlibIrFunction, KlibIrMember, KlibIrMemberAccess, KlibIrNullability, KlibIrParameter,
    KlibIrStatement, KlibIrType, KlibIrTypeId, KlibIrTypeOperator, KlibIrVariable,
};
use crate::metadata::klib_ir::{
    read_declaration_trees, KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees,
    KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::metadata::semantic::{
    semantic_ty, KotlinFunction, KotlinFunctionTypeShape, KotlinPackage, KotlinType,
};
use crate::types::{Ty, Visibility};

// --- A hand-built package `demo` --------------------------------------------------------------

/// A type a `demo` declaration or body uses, by its Kotlin class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum T {
    Int,
    Double,
    Boolean,
    String,
    Unit,
    Nothing,
    NullableAny,
}

impl T {
    fn class(self) -> &'static str {
        match self {
            T::Int => "Int",
            T::Double => "Double",
            T::Boolean => "Boolean",
            T::String => "String",
            T::Unit => "Unit",
            T::Nothing => "Nothing",
            T::NullableAny => "Any",
        }
    }

    fn nullable(self) -> bool {
        self == T::NullableAny
    }

    fn kotlin(self) -> KotlinType {
        KotlinType::Class {
            internal: format!("kotlin/{}", self.class()),
            args: Vec::new(),
            nullable: self.nullable(),
            shape: KotlinFunctionTypeShape::default(),
        }
    }

    fn semantic(self) -> Ty {
        semantic_ty(&self.kotlin(), &HashMap::new())
    }

    fn klib(self) -> KlibIrType {
        let kotlin = ["kotlin".to_owned()];
        let signature = metadata_class_signature(
            MetadataContainer {
                package: &kotlin,
                classes: &[],
                native_interop_library: false,
            },
            MetadataClass {
                name: self.class(),
                type_params: &[],
                expect: false,
            },
        );
        KlibIrType::Simple {
            classifier: public_symbol(KlibIrSymbolKind::Class, signature),
            nullability: if self.nullable() {
                KlibIrNullability::MarkedNullable
            } else {
                KlibIrNullability::NotSpecified
            },
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        }
    }
}

/// A top-level `demo` function: its name, value parameter types and result.
type Shape = (&'static str, &'static [T], T);

const PARAMETER_NAMES: [&str; 3] = ["a", "b", "c"];

fn declaration((name, params, ret): Shape) -> KotlinFunction {
    KotlinFunction {
        name: name.to_owned(),
        receiver: None,
        params: params.iter().map(|param| param.kotlin()).collect(),
        ret: ret.kotlin(),
        formals: Vec::new(),
        param_names: PARAMETER_NAMES[..params.len()]
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        param_defaults: vec![false; params.len()],
        vararg: None,
        visibility: Visibility::Public,
        modality: crate::metadata::semantic::KotlinModality::Final,
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_expect: false,
        is_external: false,
        is_static: false,
        context_count: 0,
        context_kinds: Vec::new(),
        annotations: Vec::new(),
        return_value_status: Default::default(),
        contract: None,
    }
}

/// The signature of a built-in operator declared in package `package`, inside `class` if any.
fn operator(
    package: &[&str],
    class: Option<&str>,
    name: &str,
    params: &[KotlinType],
) -> OperatorSignature {
    let package = package
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect::<Vec<_>>();
    let classes = class
        .map(|name| ClassScope {
            name,
            type_parameters: Vec::new(),
            expect: false,
        })
        .into_iter()
        .collect::<Vec<_>>();
    OperatorSignature(
        callable_signature(
            DeclarationContainer::<KotlinType> {
                package: &package,
                classes: &classes,
                native_interop_library: false,
            },
            &CallableShape {
                name,
                contexts: Vec::new(),
                receiver: None,
                params: params.iter().collect(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("a non-generic shape is signable"),
    )
}

/// A built-in operator's exact signature.
struct OperatorSignature(KlibPublicIdSignature);

fn eqeq() -> OperatorSignature {
    let any = T::NullableAny.kotlin();
    operator(
        &["kotlin", "internal", "ir"],
        None,
        "EQEQ",
        &[any.clone(), any],
    )
}

fn eqeqeq() -> OperatorSignature {
    let any = T::NullableAny.kotlin();
    operator(
        &["kotlin", "internal", "ir"],
        None,
        "EQEQEQ",
        &[any.clone(), any],
    )
}

fn ieee754equals(operand: &str) -> OperatorSignature {
    let operand = KotlinType::Class {
        internal: format!("kotlin/{operand}"),
        args: Vec::new(),
        nullable: true,
        shape: KotlinFunctionTypeShape::default(),
    };
    operator(
        &["kotlin", "internal", "ir"],
        None,
        "ieee754equals",
        &[operand.clone(), operand],
    )
}

fn not() -> OperatorSignature {
    operator(&["kotlin"], Some("Boolean"), "not", &[])
}

fn relation(name: &str) -> OperatorSignature {
    let int = kotlin_class("Int");
    operator(
        &["kotlin", "internal", "ir"],
        None,
        name,
        &[int.clone(), int],
    )
}

/// The tree builder of one `demo` function body.
struct Body {
    arena: KlibIrArena,
    types: Vec<(T, KlibIrTypeId)>,
    function: KlibIrSymbol,
    params: Vec<(KlibIrSymbol, T)>,
    signatures: HashMap<&'static str, KlibPublicIdSignature>,
    next_slot: u32,
}

impl Body {
    fn ty(&mut self, ty: T) -> KlibIrTypeId {
        if let Some((_, id)) = self.types.iter().find(|(known, _)| *known == ty) {
            return *id;
        }
        let id = self.arena.push_type(ty.klib());
        self.types.push((ty, id));
        id
    }

    fn expr(&mut self, ty: T, kind: KlibIrExprKind) -> KlibIrExprId {
        let ty = self.ty(ty);
        self.arena.push_expr(Some(ty), kind)
    }

    fn param(&mut self, index: usize) -> KlibIrExprId {
        let (symbol, ty) = self.params[index].clone();
        self.get(&symbol, ty)
    }

    fn get(&mut self, symbol: &KlibIrSymbol, ty: T) -> KlibIrExprId {
        self.expr(
            ty,
            KlibIrExprKind::GetValue {
                symbol: symbol.clone(),
                origin: None,
            },
        )
    }

    fn constant(&mut self, ty: T, constant: KlibIrConstant) -> KlibIrExprId {
        self.expr(ty, KlibIrExprKind::Const(constant))
    }

    fn int(&mut self, value: i32) -> KlibIrExprId {
        self.constant(T::Int, KlibIrConstant::Int(value))
    }

    fn call_signature(
        &mut self,
        signature: KlibPublicIdSignature,
        arguments: Vec<KlibIrExprId>,
        origin: Option<&str>,
        ty: T,
    ) -> KlibIrExprId {
        self.expr(
            ty,
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: public_symbol(KlibIrSymbolKind::Function, signature),
                    arguments: KlibIrArguments::Flat(arguments.into_iter().map(Some).collect()),
                    type_arguments: Vec::new(),
                    origin: origin.map(str::to_owned),
                },
                super_qualifier: None,
            },
        )
    }

    /// A call of the `demo` function `name`.
    fn call(&mut self, name: &str, arguments: Vec<KlibIrExprId>, ty: T) -> KlibIrExprId {
        let signature = self.signatures[name].clone();
        self.call_signature(signature, arguments, None, ty)
    }

    fn builtin(
        &mut self,
        operator: OperatorSignature,
        arguments: Vec<KlibIrExprId>,
        origin: Option<&str>,
    ) -> KlibIrExprId {
        self.call_signature(operator.0, arguments, origin, T::Boolean)
    }

    /// A `when` of `origin` with these condition branches and an `else`.
    fn when_(
        &mut self,
        origin: &str,
        branches: Vec<(KlibIrExprId, KlibIrExprId)>,
        otherwise: KlibIrExprId,
        ty: T,
    ) -> KlibIrExprId {
        let condition = self.constant(T::Boolean, KlibIrConstant::Boolean(true));
        let branches = branches
            .into_iter()
            .map(|(condition, result)| KlibIrBranch { condition, result })
            .chain([KlibIrBranch {
                condition,
                result: otherwise,
            }])
            .collect();
        self.expr(
            ty,
            KlibIrExprKind::When {
                branches,
                origin: Some(origin.to_owned()),
            },
        )
    }

    /// A local variable declaration with these serialized flags and origin.
    fn variable_with(
        &mut self,
        ty: T,
        initializer: Option<KlibIrExprId>,
        flags: u64,
        origin: &str,
    ) -> (KlibIrStatement, KlibIrSymbol) {
        self.next_slot += 1;
        let symbol = local_symbol(KlibIrSymbolKind::Variable, self.next_slot);
        let mut declaration = base(symbol.clone());
        declaration.flags = flags;
        declaration.origin = origin.to_owned();
        let ty = self.ty(ty);
        let variable = self.arena.push_variable(KlibIrVariable {
            base: declaration,
            name: format!("local{}", self.next_slot),
            ty,
            initializer,
        });
        (KlibIrStatement::Variable(variable), symbol)
    }

    fn val(&mut self, ty: T, initializer: KlibIrExprId) -> (KlibIrStatement, KlibIrSymbol) {
        self.variable_with(ty, Some(initializer), 0, "DEFINED")
    }

    fn var(&mut self, ty: T, initializer: KlibIrExprId) -> (KlibIrStatement, KlibIrSymbol) {
        self.variable_with(ty, Some(initializer), 0b10, "DEFINED")
    }

    fn set(&mut self, symbol: &KlibIrSymbol, value: KlibIrExprId) -> KlibIrStatement {
        let assignment = self.expr(
            T::Unit,
            KlibIrExprKind::SetValue {
                symbol: symbol.clone(),
                value,
                origin: Some("EQ".to_owned()),
            },
        );
        KlibIrStatement::Expression(assignment)
    }

    fn type_operator(
        &mut self,
        operator: KlibIrTypeOperator,
        argument: KlibIrExprId,
        ty: T,
    ) -> KlibIrExprId {
        let operand = self.ty(ty);
        self.expr(
            ty,
            KlibIrExprKind::TypeOperator {
                operator,
                operand,
                argument,
            },
        )
    }

    fn throw(&mut self, value: KlibIrExprId) -> KlibIrExprId {
        self.expr(T::Nothing, KlibIrExprKind::Throw(value))
    }

    fn ret(&mut self, value: KlibIrExprId) -> KlibIrStatement {
        let target = self.function.clone();
        KlibIrStatement::Expression(self.expr(T::Nothing, KlibIrExprKind::Return { target, value }))
    }
}

/// A provider publishing the `demo` functions, the frozen selection of each, and one library
/// serializing the bodies `build` makes for them (by function name; `None` for a body-less one).
struct Demo {
    facts: HashMap<&'static str, BackendCallableFact>,
    bodies: KlibDeclarationBodies,
}

fn demo(shapes: &[Shape], build: impl Fn(&str, &mut Body) -> Option<Vec<KlibIrStatement>>) -> Demo {
    let libraries = KlibLibraries::from_packages(vec![(
        vec!["demo".to_owned()],
        KotlinPackage {
            functions: shapes.iter().copied().map(declaration).collect(),
            ..KotlinPackage::default()
        },
    )])
    .expect("the declarations are signable");
    let facts = shapes
        .iter()
        .map(|(name, params, _)| {
            let params = params
                .iter()
                .map(|param| param.semantic())
                .collect::<Vec<_>>();
            (
                *name,
                frozen_function(&libraries, "demo", name, None, &params),
            )
        })
        .collect::<HashMap<_, _>>();
    let signatures = facts
        .iter()
        .map(|(name, fact)| match &fact.declaration_signature {
            Some(KlibDeclarationSignature::Public(signature)) => (*name, signature.clone()),
            _ => panic!("a top-level KLIB function freezes its public signature"),
        })
        .collect::<HashMap<_, _>>();
    let mut slot = 0;
    let trees = shapes
        .iter()
        .map(|&(name, params, ret)| {
            let function = public_symbol(KlibIrSymbolKind::Function, signatures[name].clone());
            let params = params
                .iter()
                .map(|param| {
                    slot += 1;
                    (local_symbol(KlibIrSymbolKind::ValueParameter, slot), *param)
                })
                .collect::<Vec<_>>();
            let mut body = Body {
                arena: KlibIrArena::default(),
                types: Vec::new(),
                function: function.clone(),
                params: params.clone(),
                signatures: signatures.clone(),
                next_slot: 1000 * slot,
            };
            let statements = build(name, &mut body);
            let parameters = params
                .iter()
                .zip(PARAMETER_NAMES)
                .map(|((symbol, ty), name)| KlibIrParameter {
                    base: base(symbol.clone()),
                    name: name.to_owned(),
                    ty: body.ty(*ty),
                    vararg_element_type: None,
                    default_value: None,
                })
                .collect();
            let return_type = body.ty(ret);
            let mut arena = body.arena;
            let function = arena.push_function(KlibIrFunction {
                base: base(function),
                name: name.to_owned(),
                constructor: false,
                type_parameters: Vec::new(),
                dispatch_receiver: None,
                context_parameters: Vec::new(),
                extension_receiver: None,
                regular_parameters: parameters,
                return_type,
                overridden: Vec::new(),
                companion_extension_class: None,
                prepared_inline_file: None,
                body: statements.map(KlibIrBody::Block),
            });
            KlibIrDeclarationTree {
                arena,
                declaration: KlibIrMember::Function(function),
            }
        })
        .collect();
    let trees = KlibIrModuleTrees::from_trees(trees).expect("one tree defines each identity once");
    let bodies = KlibDeclarationBodies::from_libraries(vec![trees]).expect("one library");
    Demo { facts, bodies }
}

impl Demo {
    fn callable(&self, name: &str) -> KlibCallable<'_> {
        self.facts[name]
            .klib_body_callable()
            .expect("a demo function is signed")
    }

    /// The frozen declarations of the `demo` functions `names`.
    fn callees(&self, names: &[&str]) -> KlibCalleeFacts<'_> {
        KlibCalleeFacts::new(names.iter().map(|name| self.callable(name)))
    }

    fn lower(
        &self,
        unit: &mut DependencyBodyUnit,
        name: &str,
        callees: &[&str],
    ) -> Result<FunId, KlibBodyDecline> {
        let callees = self.callees(callees);
        unit.lower_function(self.callable(name), &self.bodies, &callees)
    }

    /// The complete decline of lowering `name` with the frozen `callees`, and proof that the unit
    /// was left empty.
    fn declined(&self, name: &str, callees: &[&str]) -> String {
        let mut unit = DependencyBodyUnit::default();
        let decline = self
            .lower(&mut unit, name, callees)
            .expect_err("the body declines");
        assert_unit_is_empty(&unit);
        decline.to_string()
    }
}

fn assert_unit_is_empty(unit: &DependencyBodyUnit) {
    let ir = unit.ir();
    assert!(ir.exprs.is_empty());
    assert!(ir.functions.is_empty());
    assert!(ir.logical_types.is_empty());
    assert!(ir.binding_read_stability.is_empty());
    assert!(ir.checked_return_depths.is_empty());
    assert!(ir.callable_scopes.is_empty());
    assert!(ir.negations.is_empty());
    assert!(ir.fn_source_names.is_empty());
    assert!(ir.fn_params.is_empty());
}

/// The rendered body checked FIR lowering produces for the one function of `source` named `name`
/// with `arity` parameters.
fn source_body(source: &str, name: &str, arity: usize) -> String {
    let ir = crate::fir_lower::tests::lower_single_source_with_platform(
        source,
        "KlibBodyEquivalent",
        Box::new(crate::libraries::EmptySymbolSource),
    );
    let functions = ir
        .fn_source_names
        .iter()
        .filter(|(function, source_name)| {
            source_name.as_str() == name && ir.functions[**function as usize].params.len() == arity
        })
        .map(|(function, _)| *function)
        .collect::<Vec<_>>();
    let [function] = functions.as_slice() else {
        panic!("expected one source `{name}`, found {}", functions.len());
    };
    render(&ir, ir.functions[*function as usize].body.expect("a body"))
}

// --- Calls --------------------------------------------------------------------------------------

const IDENTITY_AND_CALLER: [Shape; 2] = [("b", &[T::Int], T::Int), ("a", &[T::Int], T::Int)];

const IDENTITY_AND_CALLER_SOURCE: &str = "fun b(a: Int): Int {\n return a\n}\n\
     fun a(a: Int): Int {\n return b(a)\n}\n";

fn identity_and_caller(name: &str, body: &mut Body) -> Option<Vec<KlibIrStatement>> {
    let a = body.param(0);
    let value = match name {
        "a" => body.call("b", vec![a], T::Int),
        _ => a,
    };
    Some(vec![body.ret(value)])
}

#[test]
fn a_call_of_a_dependency_function_lowers_its_callee_into_the_unit() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut unit = DependencyBodyUnit::default();
    let a = demo.lower(&mut unit, "a", &["a", "b"]).expect("lowers");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
    assert_eq!(
        lowered_body(&unit, a),
        source_body(IDENTITY_AND_CALLER_SOURCE, "a", 1)
    );
    assert_eq!(
        lowered_body(&unit, a),
        "scope {scope {return@Some(0) call b(v0!: Int): Int: Nothing}: Nothing}"
    );
    // The callee was lowered once, under its own signature.
    let expressions = unit.ir().exprs.len();
    let b = demo.lower(&mut unit, "b", &[]).expect("already lowered");
    assert_eq!(unit.ir().exprs.len(), expressions);
    assert_eq!(
        lowered_body(&unit, b),
        source_body(IDENTITY_AND_CALLER_SOURCE, "b", 1)
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call {
                callee: crate::ir::Callee::Local(callee),
                ..
            } => Some(*callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [b]);
}

#[test]
fn a_call_cycle_links_back_to_the_declared_function() {
    let shapes: [Shape; 2] = [("a", &[T::Int], T::Int), ("b", &[T::Int], T::Int)];
    let demo = demo(&shapes, |name, body| {
        let (relation_name, callee) = match name {
            "a" => ("less", "b"),
            _ => ("greater", "a"),
        };
        let lhs = body.param(0);
        let zero = body.int(0);
        let condition = body.builtin(relation(relation_name), vec![lhs, zero], None);
        let argument = body.param(0);
        let call = body.call(callee, vec![argument], T::Int);
        let otherwise = body.param(0);
        let conditional = body.when_("IF", vec![(condition, call)], otherwise, T::Int);
        Some(vec![body.ret(conditional)])
    });
    let source = "fun a(a: Int): Int {\n return if (a < 0) b(a) else a\n}\n\
                  fun b(a: Int): Int {\n return if (a > 0) a(a) else a\n}\n";
    let mut unit = DependencyBodyUnit::default();
    let a = demo.lower(&mut unit, "a", &["a", "b"]).expect("lowers");
    let b = demo.lower(&mut unit, "b", &[]).expect("already lowered");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
    assert_eq!(lowered_body(&unit, a), source_body(source, "a", 1));
    assert_eq!(lowered_body(&unit, b), source_body(source, "b", 1));
}

#[test]
fn a_declining_callee_leaves_the_unit_as_it_was() {
    let shapes: [Shape; 3] = [
        ("a", &[T::Int], T::Int),
        ("b", &[T::Int], T::Int),
        ("c", &[T::Int], T::Int),
    ];
    let demo = demo(&shapes, |name, body| {
        let a = body.param(0);
        let value = match name {
            "a" => body.call("b", vec![a], T::Int),
            "b" => body.throw(a),
            _ => a,
        };
        Some(vec![body.ret(value)])
    });
    let mut unit = DependencyBodyUnit::default();
    let c = demo.lower(&mut unit, "c", &[]).expect("lowers");
    let before = lowered_body(&unit, c);
    let expressions = unit.ir().exprs.len();
    let decline = demo
        .lower(&mut unit, "a", &["b"])
        .expect_err("the callee throws");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.b` (it uses `throw`))"
    );
    assert_eq!(unit.ir().functions.len(), 1);
    assert_eq!(unit.ir().exprs.len(), expressions);
    assert_eq!(lowered_body(&unit, c), before);
    validated(unit.ir());
    // Neither the caller nor the callee is remembered: lowering the callee alone declines by its
    // own form.
    assert_eq!(
        demo.lower(&mut unit, "b", &[])
            .expect_err("it throws")
            .to_string(),
        "the KLIB body of `demo.b` (it uses `throw`)"
    );
}

#[test]
fn a_callee_without_a_body_declines_by_its_own_reason() {
    let demo = demo(&IDENTITY_AND_CALLER, |name, body| match name {
        "a" => identity_and_caller(name, body),
        _ => None,
    });
    assert_eq!(
        demo.declined("a", &["b"]),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.b` \
         (its library serializes none))"
    );
}

#[test]
fn a_callee_no_frozen_selection_describes_declines_by_name() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    assert_eq!(
        demo.declined("a", &["a"]),
        "the KLIB body of `demo.a` (it calls `demo.b`, which no selected declaration describes)"
    );
}

#[test]
fn a_callee_two_frozen_selections_describe_declines_by_name() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    assert_eq!(
        demo.declined("a", &["b", "b"]),
        "the KLIB body of `demo.a` (it calls `demo.b`, which two selected declarations describe)"
    );
}

#[test]
fn a_call_of_an_already_lowered_function_still_needs_its_frozen_selection() {
    let shapes: [Shape; 3] = [
        ("a", &[T::Int], T::Int),
        ("b", &[T::Int], T::Int),
        ("c", &[T::Int], T::Int),
    ];
    let demo = demo(&shapes, |name, body| {
        let a = body.param(0);
        let value = match name {
            "b" => a,
            _ => body.call("b", vec![a], T::Int),
        };
        Some(vec![body.ret(value)])
    });
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "a", &["b"]).expect("lowers");
    let functions = unit.ir().functions.len();
    let expressions = unit.ir().exprs.len();
    assert_eq!(
        demo.lower(&mut unit, "c", &[])
            .expect_err("`b` is not selected for this lowering")
            .to_string(),
        "the KLIB body of `demo.c` (it calls `demo.b`, which no selected declaration describes)"
    );
    assert_eq!(
        demo.lower(&mut unit, "c", &["b", "b"])
            .expect_err("`b` is ambiguous for this lowering")
            .to_string(),
        "the KLIB body of `demo.c` (it calls `demo.b`, which two selected declarations describe)"
    );
    assert_eq!(unit.ir().functions.len(), functions);
    assert_eq!(unit.ir().exprs.len(), expressions);
    validated(unit.ir());
}

// --- Equality and negation ----------------------------------------------------------------------

/// `f(a, b) = a OP b`, with the built-in call `build` makes of the two parameter reads.
fn binary(
    params: &'static [T],
    build: impl Fn(&mut Body, KlibIrExprId, KlibIrExprId) -> KlibIrExprId,
) -> Demo {
    demo(&[("f", params, T::Boolean)], |_, body| {
        let lhs = body.param(0);
        let rhs = body.param(1);
        let value = build(body, lhs, rhs);
        Some(vec![body.ret(value)])
    })
}

fn lowered_f(demo: &Demo) -> String {
    let mut unit = DependencyBodyUnit::default();
    let f = demo
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    lowered_body(&unit, f)
}

#[test]
fn equality_built_ins_lower_as_the_source_operators_do() {
    let int: &[T] = &[T::Int, T::Int];
    let double: &[T] = &[T::Double, T::Double];
    let string: &[T] = &[T::String, T::String];
    let any: &[T] = &[T::NullableAny, T::NullableAny];
    type Build = fn(&mut Body, KlibIrExprId, KlibIrExprId) -> KlibIrExprId;
    let cases: [(&'static [T], &str, &str, Build); 8] = [
        (int, "Int", "a == b", |body, a, b| {
            body.builtin(eqeq(), vec![a, b], Some("EQEQ"))
        }),
        (int, "Int", "a != b", |body, a, b| {
            let equal = body.builtin(eqeq(), vec![a, b], Some("EXCLEQ"));
            body.builtin(not(), vec![equal], Some("EXCLEQ"))
        }),
        (int, "Int", "!(a == b)", |body, a, b| {
            let equal = body.builtin(eqeq(), vec![a, b], Some("EQEQ"));
            body.builtin(not(), vec![equal], None)
        }),
        (double, "Double", "a == b", |body, a, b| {
            body.builtin(ieee754equals("Double"), vec![a, b], Some("EQEQ"))
        }),
        (double, "Double", "a != b", |body, a, b| {
            let equal = body.builtin(ieee754equals("Double"), vec![a, b], Some("EXCLEQ"));
            body.builtin(not(), vec![equal], Some("EXCLEQ"))
        }),
        (string, "String", "a == b", |body, a, b| {
            body.builtin(eqeq(), vec![a, b], Some("EQEQ"))
        }),
        (any, "Any?", "a === b", |body, a, b| {
            body.builtin(eqeqeq(), vec![a, b], Some("EQEQEQ"))
        }),
        (any, "Any?", "a !== b", |body, a, b| {
            let identical = body.builtin(eqeqeq(), vec![a, b], Some("EXCLEQEQ"));
            body.builtin(not(), vec![identical], Some("EXCLEQEQ"))
        }),
    ];
    for (params, source_type, operator, build) in cases {
        let source = format!(
            "fun f(a: {source_type}, b: {source_type}): Boolean {{\n return {operator}\n}}\n"
        );
        assert_eq!(
            lowered_f(&binary(params, build)),
            source_body(&source, "f", 2),
            "{operator} on {source_type}"
        );
    }
}

#[test]
fn negation_lowers_as_the_source_operator_does() {
    let demo = demo(&[("f", &[T::Boolean], T::Boolean)], |_, body| {
        let a = body.param(0);
        let negated = body.builtin(not(), vec![a], None);
        Some(vec![body.ret(negated)])
    });
    let lowered = lowered_f(&demo);
    assert_eq!(
        lowered,
        source_body("fun f(a: Boolean): Boolean {\n return !a\n}\n", "f", 1)
    );
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) negation Eq(v0!: Boolean, Boolean(false)): Boolean: \
         Nothing}: Nothing}"
    );
}

#[test]
fn an_equality_the_checker_would_classify_otherwise_declines() {
    let doubles = binary(&[T::Double, T::Double], |body, a, b| {
        body.builtin(eqeq(), vec![a, b], Some("EQEQ"))
    });
    assert_eq!(
        doubles.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         `EQEQ` compares two operands of one floating-point type)"
    );
    let ints = binary(&[T::Int, T::Int], |body, a, b| {
        body.builtin(ieee754equals("Double"), vec![a, b], Some("EQEQ"))
    });
    assert_eq!(
        ints.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses an IEEE 754 equality of operands other than one \
         floating-point type)"
    );
    let float_operator = binary(&[T::Double, T::Double], |body, a, b| {
        body.builtin(ieee754equals("Float"), vec![a, b], Some("EQEQ"))
    });
    assert_eq!(
        float_operator.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         `ieee754equals` compares operands of another type than its own)"
    );
}

#[test]
fn a_negation_of_another_source_form_declines() {
    let not_in = demo(&[("f", &[T::Boolean], T::Boolean)], |_, body| {
        let a = body.param(0);
        let negated = body.builtin(not(), vec![a], Some("NOT_IN"));
        Some(vec![body.ret(negated)])
    });
    assert_eq!(
        not_in.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a negation of another form than `!`, `!=` or `!==`)"
    );
    let bare = demo(&[("f", &[T::Boolean], T::Boolean)], |_, body| {
        let a = body.param(0);
        let negated = body.builtin(not(), vec![a], Some("EXCLEQ"));
        Some(vec![body.ret(negated)])
    });
    assert_eq!(
        bare.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         a `!=` negates no built-in equality)"
    );
}

// --- Local variables ----------------------------------------------------------------------------

#[test]
fn local_variables_lower_as_the_source_declarations_do() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let (x_declaration, x) = body.val(T::Int, a);
        let x_read = body.get(&x, T::Int);
        let (y_declaration, y) = body.var(T::Int, x_read);
        let a = body.param(0);
        let assignment = body.set(&y, a);
        let y_read = body.get(&y, T::Int);
        Some(vec![
            x_declaration,
            y_declaration,
            assignment,
            body.ret(y_read),
        ])
    });
    let lowered = lowered_f(&demo);
    assert_eq!(
        lowered,
        source_body(
            "fun f(a: Int): Int {\n val x = a\n var y = x\n y = a\n return y\n}\n",
            "f",
            1
        )
    );
    assert_eq!(
        lowered,
        "scope {scope {val v1: Int = v0!: Int; val v2: Int = v1!: Int; v2 = v0!: Int: Unit; \
         return@Some(0) v2~: Int: Nothing}: Nothing}"
    );
}

#[test]
fn unmodelled_local_variables_decline_by_form() {
    let cases: [(u64, &str, bool, &str); 4] = [
        (
            0b1000 | 0b10,
            "DEFINED",
            true,
            "it uses a `lateinit` variable",
        ),
        (0b100, "DEFINED", true, "it uses a `const` variable"),
        (
            0,
            "IR_TEMPORARY_VARIABLE",
            true,
            "it uses a compiler-introduced variable",
        ),
        (
            0,
            "DEFINED",
            false,
            "it uses a variable without an initializer",
        ),
    ];
    for (flags, origin, initialized, reason) in cases {
        let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
            let a = body.param(0);
            let (declaration, x) =
                body.variable_with(T::Int, initialized.then_some(a), flags, origin);
            let read = body.get(&x, T::Int);
            Some(vec![declaration, body.ret(read)])
        });
        assert_eq!(
            demo.declined("f", &[]),
            format!("the KLIB body of `demo.f` ({reason})")
        );
    }
}

#[test]
fn an_assignment_to_a_val_declines() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let (declaration, x) = body.val(T::Int, a);
        let a = body.param(0);
        let assignment = body.set(&x, a);
        let read = body.get(&x, T::Int);
        Some(vec![declaration, assignment, body.ret(read)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses an assignment that initializes a `val`)"
    );
}

// --- Implicit casts -----------------------------------------------------------------------------

#[test]
fn an_implicit_cast_to_the_value_type_adds_nothing() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, T::Int);
        Some(vec![body.ret(cast)])
    });
    assert_eq!(
        lowered_f(&demo),
        source_body("fun f(a: Int): Int {\n return a\n}\n", "f", 1)
    );
}

#[test]
fn a_converting_or_unmodelled_type_operator_declines() {
    let converting = demo(&[("f", &[T::NullableAny], T::NullableAny)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, T::String);
        let widened = body.type_operator(KlibIrTypeOperator::ImplicitCast, cast, T::NullableAny);
        Some(vec![body.ret(widened)])
    });
    assert_eq!(
        converting.declined("f", &[]),
        "the KLIB body of `demo.f` (it converts a value implicitly)"
    );
    let not_null = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitNotNull, a, T::Int);
        Some(vec![body.ret(cast)])
    });
    assert_eq!(
        not_null.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses an implicit non-null assertion)"
    );
}

// --- Multi-branch `when` ------------------------------------------------------------------------

/// `f(a)` returning -1, 1 or 0 by the sign of `a`, as a `when` of `origin`.
fn sign(origin: &'static str) -> Demo {
    demo(&[("f", &[T::Int], T::Int)], move |_, body| {
        let a = body.param(0);
        let zero = body.int(0);
        let negative = body.builtin(relation("less"), vec![a, zero], Some("LT"));
        let minus_one = body.int(-1);
        let a = body.param(0);
        let zero = body.int(0);
        let positive = body.builtin(relation("greater"), vec![a, zero], Some("GT"));
        let one = body.int(1);
        let otherwise = body.int(0);
        let when = body.when_(
            origin,
            vec![(negative, minus_one), (positive, one)],
            otherwise,
            T::Int,
        );
        Some(vec![body.ret(when)])
    })
}

#[test]
fn a_flat_when_lowers_as_the_source_when_does() {
    let source =
        "fun f(a: Int): Int {\n return when {\n a < 0 -> -1\n a > 0 -> 1\n else -> 0\n }\n}\n";
    let lowered = lowered_f(&sign("WHEN"));
    assert_eq!(lowered, source_body(source, "f", 1));
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) exhaustive:Int when[Lt(v0!: Int, Int(0): Int): Boolean -> \
         Int(-1): Int, Gt(v0!: Int, Int(0): Int): Boolean -> Int(1): Int, else -> Int(0): Int]: \
         Int: Nothing}: Nothing}"
    );
}

#[test]
fn a_flat_if_chain_lowers_as_the_source_else_if_chain_does() {
    let source = "fun f(a: Int): Int {\n return if (a < 0) -1 else if (a > 0) 1 else 0\n}\n";
    assert_eq!(lowered_f(&sign("IF")), source_body(source, "f", 1));
}

#[test]
fn a_short_circuit_or_a_when_without_else_declines() {
    let and = demo(
        &[("f", &[T::Boolean, T::Boolean], T::Boolean)],
        |_, body| {
            let a = body.param(0);
            let b = body.param(1);
            let otherwise = body.constant(T::Boolean, KlibIrConstant::Boolean(false));
            let when = body.when_("ANDAND", vec![(a, b)], otherwise, T::Boolean);
            Some(vec![body.ret(when)])
        },
    );
    assert_eq!(
        and.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses `&&`)"
    );
    let without_else = demo(&[("f", &[T::Boolean], T::Int)], |_, body| {
        let a = body.param(0);
        let one = body.int(1);
        let b = body.param(0);
        let two = body.int(2);
        let when = body.expr(
            T::Int,
            KlibIrExprKind::When {
                branches: vec![
                    KlibIrBranch {
                        condition: a,
                        result: one,
                    },
                    KlibIrBranch {
                        condition: b,
                        result: two,
                    },
                ],
                origin: Some("WHEN".to_owned()),
            },
        );
        Some(vec![body.ret(when)])
    });
    assert_eq!(
        without_else.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a `when` without an `else`)"
    );
}

// --- The Kotlin/Native stdlib -------------------------------------------------------------------

/// The semantic type of the class `kotlin.<name>`.
fn kotlin(name: &str) -> Ty {
    semantic_ty(&kotlin_class(name), &HashMap::new())
}

#[test]
fn stdlib_structural_equality_lowers_as_its_source_does() {
    let Some(root) = distribution_root() else {
        return;
    };
    let (libraries, bodies) = stdlib(&root);
    let any = kotlin("Any");
    let fact = frozen_function(
        &libraries,
        "kotlin/native/internal",
        "Kotlin_equals",
        None,
        &[any, any],
    );
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun Kotlin_equals(a: Any, b: Any): Boolean {\n return a == b\n}\n";
    let lowered = lowered_body(&unit, function);
    assert_eq!(lowered, source_body(source, "Kotlin_equals", 2));
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) Eq.Structural(v0!: kotlin/Any, v1!: kotlin/Any): Boolean: Nothing}: \
         Nothing}"
    );
}

/// `kotlin.math.max` on `ULong` calls `kotlin.comparisons.maxOf`, whose body the stdlib
/// serializes with an inlined function block: the caller declines through its callee when the
/// callee is selected, and by the callee's identity when it is not.
#[test]
fn stdlib_unsigned_max_declines_through_its_callee() {
    let Some(root) = distribution_root() else {
        return;
    };
    let (libraries, bodies) = stdlib(&root);
    let unsigned = kotlin("ULong");
    let max = frozen_function(
        &libraries,
        "kotlin/math",
        "max",
        None,
        &[unsigned, unsigned],
    );
    let max_of = frozen_function(
        &libraries,
        "kotlin/comparisons",
        "maxOf",
        None,
        &[unsigned, unsigned],
    );
    let callable = max.klib_body_callable().expect("signed");
    let mut unit = DependencyBodyUnit::default();
    let selected = KlibCalleeFacts::new([max_of.klib_body_callable().expect("signed")]);
    assert_eq!(
        unit.lower_function(callable, &bodies, &selected)
            .expect_err("the callee declines")
            .to_string(),
        "the KLIB body of `kotlin.math.max` \
         (it reaches the KLIB body of `kotlin.comparisons.maxOf` (it uses a block))"
    );
    assert_unit_is_empty(&unit);
    assert_eq!(
        unit.lower_function(callable, &bodies, &KlibCalleeFacts::default())
            .expect_err("the callee is not selected")
            .to_string(),
        "the KLIB body of `kotlin.math.max` \
         (it calls `kotlin.comparisons.maxOf`, which no selected declaration describes)"
    );
    assert_unit_is_empty(&unit);
}

/// Every call the stdlib serializes of a built-in operator lowering signs passes operands of the
/// operator's own type: the table's identities are the stdlib's, operand type for operand type.
#[test]
fn stdlib_built_in_calls_have_the_signed_operands() {
    let Some(root) = distribution_root() else {
        return;
    };
    let path = root.join("klib/common/stdlib");
    let archive = crate::klib::KlibArchive::open(&path).expect("the stdlib opens");
    let trees = read_declaration_trees(&archive).expect("the stdlib IR decodes");
    let builtins = super::super::ir_builtins::IrBuiltinOperators::new();
    let mut checked = HashMap::<&'static str, usize>::new();
    for tree in trees.trees() {
        let arena = &tree.arena;
        for index in 0..arena.expr_count() {
            let KlibIrExprKind::Call { access, .. } = &arena.expr(expr_id(arena, index)).kind
            else {
                continue;
            };
            let KlibIrSignature::Public(callee) = &access.symbol.signature else {
                continue;
            };
            let Some(operator) = builtins.operator(callee) else {
                continue;
            };
            let KlibIrArguments::Flat(arguments) = &access.arguments else {
                panic!("the 2.4.20 stdlib serializes flat arguments");
            };
            let operand_types = arguments
                .iter()
                .map(|argument| {
                    let argument = argument.expect("a built-in call supplies every argument");
                    let ty = arena.expr(argument).ty.expect("an argument is typed");
                    super::super::klib_types::semantic_type(arena, ty).ok()
                })
                .collect::<Vec<_>>();
            let (kind, expected) = match operator {
                super::super::ir_builtins::BuiltinOperator::Relation(relation) => {
                    ("relation", Some(relation.operand))
                }
                super::super::ir_builtins::BuiltinOperator::Ieee754Equals { operand } => {
                    ("ieee754equals", Some(operand))
                }
                super::super::ir_builtins::BuiltinOperator::Not => ("not", Some(Ty::Boolean)),
                super::super::ir_builtins::BuiltinOperator::Equals => ("EQEQ", None),
                super::super::ir_builtins::BuiltinOperator::Identical => ("EQEQEQ", None),
            };
            if let Some(expected) = expected {
                for ty in operand_types.into_iter().flatten() {
                    assert_eq!(ty.non_null(), expected, "{kind} {callee:?}");
                }
            }
            *checked.entry(kind).or_default() += 1;
        }
    }
    for kind in ["relation", "ieee754equals", "not", "EQEQ", "EQEQEQ"] {
        assert!(
            checked.get(kind).copied().unwrap_or(0) > 0,
            "no {kind} call: {checked:?}"
        );
    }
}

fn expr_id(arena: &KlibIrArena, index: usize) -> KlibIrExprId {
    arena.expr_id(index)
}

/// The local-variable flag layout lowering reads is the serializer's: every stdlib local that is
/// initialized and assigned again is flagged `var`, and the annotations flag is set exactly on the
/// locals that carry annotations.
#[test]
fn stdlib_local_variable_flags_have_the_decoded_layout() {
    let Some(root) = distribution_root() else {
        return;
    };
    let path = root.join("klib/common/stdlib");
    let archive = crate::klib::KlibArchive::open(&path).expect("the stdlib opens");
    let trees = read_declaration_trees(&archive).expect("the stdlib IR decodes");
    let (mut reassigned, mut annotated) = (0, 0);
    for tree in trees.trees() {
        let arena = &tree.arena;
        let assigned = (0..arena.expr_count())
            .filter_map(|index| match &arena.expr(expr_id(arena, index)).kind {
                KlibIrExprKind::SetValue { symbol, .. } => Some(symbol.clone()),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for index in 0..arena.variable_count() {
            let variable = arena.variable(arena.variable_id(index));
            let flags = variable.base.flags;
            assert_eq!(
                flags & 1 != 0,
                !variable.base.annotations.is_empty(),
                "{:?} flags {flags:#x}",
                variable.base.symbol
            );
            annotated += usize::from(flags & 1 != 0);
            if variable.initializer.is_some() && assigned.contains(&variable.base.symbol) {
                assert_eq!(
                    flags & 0b10,
                    0b10,
                    "{:?} is reassigned",
                    variable.base.symbol
                );
                reassigned += 1;
            }
        }
    }
    assert!(reassigned > 0 && annotated > 0, "{reassigned} {annotated}");
}

/// Every value node of a lowered body has a checked type a target can read: a backend that
/// types each value it emits (Wasm) never meets an untyped one. A local declaration is a
/// statement, and a function's body root is the callable's own scope; neither is a value.
#[test]
fn every_lowered_value_has_a_checked_type() {
    let shapes: [Shape; 3] = [
        ("callee", &[T::Int], T::Int),
        ("f", &[T::Int, T::Double], T::Boolean),
        ("g", &[T::Int], T::Int),
    ];
    let demo = demo(&shapes, |name, body| match name {
        "callee" => {
            let a = body.param(0);
            Some(vec![body.ret(a)])
        }
        "f" => {
            let a = body.param(0);
            let (x_declaration, x) = body.var(T::Int, a);
            let a = body.param(0);
            let called = body.call("callee", vec![a], T::Int);
            let assignment = body.set(&x, called);
            let x_read = body.get(&x, T::Int);
            let zero = body.int(0);
            let equal = body.builtin(eqeq(), vec![x_read, zero], Some("EXCLEQ"));
            let different = body.builtin(not(), vec![equal], Some("EXCLEQ"));
            let b = body.param(1);
            let c = body.param(1);
            let nan = body.builtin(ieee754equals("Double"), vec![b, c], Some("EQEQ"));
            let not_nan = body.builtin(not(), vec![nan], None);
            let y_read = body.get(&x, T::Int);
            let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, y_read, T::Int);
            let zero = body.int(0);
            let positive = body.builtin(relation("greater"), vec![cast, zero], Some("GT"));
            let otherwise = body.constant(T::Boolean, KlibIrConstant::Boolean(false));
            let when = body.when_(
                "IF",
                vec![(different, not_nan), (positive, not_nan)],
                otherwise,
                T::Boolean,
            );
            Some(vec![x_declaration, assignment, body.ret(when)])
        }
        _ => None,
    });
    let mut unit = DependencyBodyUnit::default();
    let f = demo
        .lower(&mut unit, "f", &["callee"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    let g = sign("WHEN");
    let g_function = g
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let ir = unit.ir();
    let roots = ir
        .functions
        .iter()
        .map(|function| function.body.expect("a lowered function has a body"))
        .collect::<Vec<_>>();
    assert_eq!(ir.functions.len(), 3, "{f} {g_function}");
    let untyped = (0..ir.exprs.len())
        .map(|index| crate::ir::ExprId::try_from(index).expect("fits"))
        .filter(|expression| !roots.contains(expression))
        .filter(|expression| !matches!(ir.expr(*expression), crate::ir::IrExpr::Variable { .. }))
        .filter(|expression| ir.checked_type(*expression).is_none())
        .map(|expression| format!("{:?}", ir.expr(expression)))
        .collect::<Vec<_>>();
    assert_eq!(untyped, Vec::<String>::new());
}
