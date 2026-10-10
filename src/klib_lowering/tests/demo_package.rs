//! The hand-built package `demo` the body-form tests lower: a provider publishing its top-level
//! functions, the frozen selection of each, and one library serializing the bodies a test builds
//! for them, with a tree builder for those bodies and the checked FIR lowering of equivalent
//! source to compare with.

use std::collections::HashMap;

use super::super::*;
use super::{
    base, frozen_function, kotlin_class, local_symbol, lowered_body, public_symbol, render,
    validated,
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
    KlibIrFunction, KlibIrLoop, KlibIrMember, KlibIrMemberAccess, KlibIrNullability,
    KlibIrParameter, KlibIrStatement, KlibIrType, KlibIrTypeId, KlibIrTypeOperator, KlibIrVariable,
};
use crate::metadata::klib_ir::{
    KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::metadata::semantic::{
    semantic_ty, KotlinFunction, KotlinFunctionTypeShape, KotlinPackage, KotlinType,
};
use crate::types::{Ty, Visibility};

/// A type a `demo` declaration or body uses, by its Kotlin class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum T {
    Int,
    Double,
    Boolean,
    String,
    Unit,
    Nothing,
    Throwable,
    Any,
    NullableAny,
    NullableInt,
    NullableString,
    /// `Nothing?`, the type of `null`.
    NullableNothing,
}

impl T {
    pub(super) fn class(self) -> &'static str {
        match self {
            T::Int => "Int",
            T::Double => "Double",
            T::Boolean => "Boolean",
            T::String => "String",
            T::Unit => "Unit",
            T::Nothing => "Nothing",
            T::Throwable => "Throwable",
            T::Any => "Any",
            T::NullableAny => "Any",
            T::NullableInt => "Int",
            T::NullableString => "String",
            T::NullableNothing => "Nothing",
        }
    }

    pub(super) fn nullable(self) -> bool {
        matches!(
            self,
            T::NullableAny | T::NullableInt | T::NullableString | T::NullableNothing
        )
    }

    pub(super) fn kotlin(self) -> KotlinType {
        KotlinType::Class {
            internal: format!("kotlin/{}", self.class()),
            args: Vec::new(),
            nullable: self.nullable(),
            shape: KotlinFunctionTypeShape::default(),
        }
    }

    pub(super) fn semantic(self) -> Ty {
        semantic_ty(&self.kotlin(), &HashMap::new())
    }

    pub(super) fn klib(self) -> KlibIrType {
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
pub(super) type Shape = (&'static str, &'static [T], T);

const PARAMETER_NAMES: [&str; 3] = ["a", "b", "c"];

pub(super) fn declaration((name, params, ret): Shape) -> KotlinFunction {
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
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_expect: false,
        is_static: false,
        context_count: 0,
        context_kinds: Vec::new(),
        annotations: Vec::new(),
    }
}

/// The signature of a built-in operator declared in package `package`, inside `class` if any.
pub(super) fn operator(
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
pub(super) struct OperatorSignature(pub(super) KlibPublicIdSignature);

pub(super) fn eqeq() -> OperatorSignature {
    let any = T::NullableAny.kotlin();
    operator(
        &["kotlin", "internal", "ir"],
        None,
        "EQEQ",
        &[any.clone(), any],
    )
}

pub(super) fn eqeqeq() -> OperatorSignature {
    let any = T::NullableAny.kotlin();
    operator(
        &["kotlin", "internal", "ir"],
        None,
        "EQEQEQ",
        &[any.clone(), any],
    )
}

pub(super) fn ieee754equals(operand: &str) -> OperatorSignature {
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

pub(super) fn not() -> OperatorSignature {
    operator(&["kotlin"], Some("Boolean"), "not", &[])
}

pub(super) fn relation(name: &str) -> OperatorSignature {
    let int = kotlin_class("Int");
    operator(
        &["kotlin", "internal", "ir"],
        None,
        name,
        &[int.clone(), int],
    )
}

/// The tree builder of one `demo` function body.
pub(super) struct Body {
    pub(super) arena: KlibIrArena,
    pub(super) types: Vec<(T, KlibIrTypeId)>,
    pub(super) function: KlibIrSymbol,
    pub(super) params: Vec<(KlibIrSymbol, T)>,
    pub(super) signatures: HashMap<&'static str, KlibPublicIdSignature>,
    pub(super) next_slot: u32,
}

impl Body {
    pub(super) fn ty(&mut self, ty: T) -> KlibIrTypeId {
        if let Some((_, id)) = self.types.iter().find(|(known, _)| *known == ty) {
            return *id;
        }
        let id = self.arena.push_type(ty.klib());
        self.types.push((ty, id));
        id
    }

    pub(super) fn expr(&mut self, ty: T, kind: KlibIrExprKind) -> KlibIrExprId {
        let ty = self.ty(ty);
        self.arena.push_expr(Some(ty), kind)
    }

    pub(super) fn param(&mut self, index: usize) -> KlibIrExprId {
        let (symbol, ty) = self.params[index].clone();
        self.get(&symbol, ty)
    }

    pub(super) fn get(&mut self, symbol: &KlibIrSymbol, ty: T) -> KlibIrExprId {
        self.expr(
            ty,
            KlibIrExprKind::GetValue {
                symbol: symbol.clone(),
                origin: None,
            },
        )
    }

    pub(super) fn constant(&mut self, ty: T, constant: KlibIrConstant) -> KlibIrExprId {
        self.expr(ty, KlibIrExprKind::Const(constant))
    }

    pub(super) fn int(&mut self, value: i32) -> KlibIrExprId {
        self.constant(T::Int, KlibIrConstant::Int(value))
    }

    pub(super) fn call_signature(
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
    pub(super) fn call(&mut self, name: &str, arguments: Vec<KlibIrExprId>, ty: T) -> KlibIrExprId {
        let signature = self.signatures[name].clone();
        self.call_signature(signature, arguments, None, ty)
    }

    pub(super) fn builtin(
        &mut self,
        operator: OperatorSignature,
        arguments: Vec<KlibIrExprId>,
        origin: Option<&str>,
    ) -> KlibIrExprId {
        self.call_signature(operator.0, arguments, origin, T::Boolean)
    }

    /// A `when` of `origin` with these condition branches and an `else`.
    pub(super) fn when_(
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
    pub(super) fn variable_with(
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

    pub(super) fn val(
        &mut self,
        ty: T,
        initializer: KlibIrExprId,
    ) -> (KlibIrStatement, KlibIrSymbol) {
        self.variable_with(ty, Some(initializer), 0, "DEFINED")
    }

    pub(super) fn var(
        &mut self,
        ty: T,
        initializer: KlibIrExprId,
    ) -> (KlibIrStatement, KlibIrSymbol) {
        self.variable_with(ty, Some(initializer), 0b10, "DEFINED")
    }

    pub(super) fn set(&mut self, symbol: &KlibIrSymbol, value: KlibIrExprId) -> KlibIrStatement {
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

    pub(super) fn type_operator(
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

    pub(super) fn throw(&mut self, value: KlibIrExprId) -> KlibIrExprId {
        self.expr(T::Nothing, KlibIrExprKind::Throw(value))
    }

    /// A `when` of `origin` with exactly these branches, an `else` included only if one of them
    /// has the constant `true` condition [`Self::otherwise`] makes.
    pub(super) fn branches(
        &mut self,
        origin: &str,
        branches: Vec<(KlibIrExprId, KlibIrExprId)>,
        ty: T,
    ) -> KlibIrExprId {
        let branches = branches
            .into_iter()
            .map(|(condition, result)| KlibIrBranch { condition, result })
            .collect();
        self.expr(
            ty,
            KlibIrExprKind::When {
                branches,
                origin: Some(origin.to_owned()),
            },
        )
    }

    /// The condition of a KLIB `else` branch.
    pub(super) fn otherwise(&mut self) -> KlibIrExprId {
        self.constant(T::Boolean, KlibIrConstant::Boolean(true))
    }

    pub(super) fn boolean(&mut self, value: bool) -> KlibIrExprId {
        self.constant(T::Boolean, KlibIrConstant::Boolean(value))
    }

    pub(super) fn string(&mut self, value: &str) -> KlibIrExprId {
        self.constant(T::String, KlibIrConstant::String(value.to_owned()))
    }

    /// A plain block of `statements`, typed `ty` as a KLIB types it by its use.
    pub(super) fn block(&mut self, statements: Vec<KlibIrStatement>, ty: T) -> KlibIrExprId {
        self.expr(
            ty,
            KlibIrExprKind::Block {
                statements,
                origin: None,
            },
        )
    }

    /// `value` coerced to `Unit`, as a KLIB states a discarded value of another type.
    pub(super) fn coerced(&mut self, value: KlibIrExprId) -> KlibIrExprId {
        self.type_operator(KlibIrTypeOperator::ImplicitCoercionToUnit, value, T::Unit)
    }

    pub(super) fn concat(&mut self, parts: Vec<KlibIrExprId>) -> KlibIrExprId {
        self.expr(T::String, KlibIrExprKind::StringConcat(parts))
    }

    /// A source `while` (or, `post_test`, `do`-`while`) loop with the identity `id`.
    pub(super) fn loop_(
        &mut self,
        id: u32,
        condition: KlibIrExprId,
        body: KlibIrExprId,
        post_test: bool,
    ) -> KlibIrStatement {
        let serialized = KlibIrLoop {
            id,
            condition,
            body: Some(body),
            label: None,
            origin: Some(
                if post_test {
                    "DO_WHILE_LOOP"
                } else {
                    "WHILE_LOOP"
                }
                .to_owned(),
            ),
        };
        let kind = if post_test {
            KlibIrExprKind::DoWhile(serialized)
        } else {
            KlibIrExprKind::While(serialized)
        };
        KlibIrStatement::Expression(self.expr(T::Unit, kind))
    }

    pub(super) fn break_(&mut self, loop_id: u32) -> KlibIrExprId {
        self.expr(
            T::Nothing,
            KlibIrExprKind::Break {
                loop_id,
                label: None,
            },
        )
    }

    pub(super) fn continue_(&mut self, loop_id: u32) -> KlibIrExprId {
        self.expr(
            T::Nothing,
            KlibIrExprKind::Continue {
                loop_id,
                label: None,
            },
        )
    }

    /// The `return` of `value` as an expression rather than a statement.
    pub(super) fn return_(&mut self, value: KlibIrExprId) -> KlibIrExprId {
        let target = self.function.clone();
        self.expr(T::Nothing, KlibIrExprKind::Return { target, value })
    }

    pub(super) fn ret(&mut self, value: KlibIrExprId) -> KlibIrStatement {
        let target = self.function.clone();
        KlibIrStatement::Expression(self.expr(T::Nothing, KlibIrExprKind::Return { target, value }))
    }
}

/// A provider publishing the `demo` functions, the frozen selection of each, and one library
/// serializing the bodies `build` makes for them (by function name; `None` for a body-less one).
pub(super) struct Demo {
    pub(super) facts: HashMap<&'static str, BackendCallableFact>,
    pub(super) bodies: KlibDeclarationBodies,
}

pub(super) fn demo(
    shapes: &[Shape],
    build: impl Fn(&str, &mut Body) -> Option<Vec<KlibIrStatement>>,
) -> Demo {
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
    pub(super) fn callable(&self, name: &str) -> KlibCallable<'_> {
        self.facts[name]
            .klib_body_callable()
            .expect("a demo function is signed")
    }

    /// The frozen declarations of the `demo` functions `names`.
    pub(super) fn callees(&self, names: &[&str]) -> KlibCalleeFacts<'_> {
        KlibCalleeFacts::new(names.iter().map(|name| self.callable(name)))
    }

    pub(super) fn lower(
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
    pub(super) fn declined(&self, name: &str, callees: &[&str]) -> String {
        let mut unit = DependencyBodyUnit::default();
        let decline = self
            .lower(&mut unit, name, callees)
            .expect_err("the body declines");
        assert_unit_is_empty(&unit);
        decline.to_string()
    }
}

pub(super) fn assert_unit_is_empty(unit: &DependencyBodyUnit) {
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
pub(super) fn source_body(source: &str, name: &str, arity: usize) -> String {
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

pub(super) fn lowered_f(demo: &Demo) -> String {
    let mut unit = DependencyBodyUnit::default();
    let f = demo
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    lowered_body(&unit, f)
}
