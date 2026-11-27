//! A function body as Kotlin's serialized IR records it, built from checked common IR.
//!
//! The KLIB writer records target-neutral Kotlin bodies, so a body is translated before any target
//! preparation. Each node becomes the Kotlin IR form kotlinc's own serializer writes for it: a
//! primitive relation is a call to a compiler built-in operator (`kotlin.internal.ir/less`), an
//! equality is `EQEQ`, `!!` is `CHECK_NOT_NULL`, and every call names its callee by exact
//! signature. A body holding any node this writer does not translate yet has no tree, and its
//! declaration stays metadata-only.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    Callee, EqualityMode, ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile,
    IrIntrinsic, IrPropertyDispatch, IrTypeOp, IrTypeParameter,
};
use crate::klib_lowering::ir_builtins::{BuiltinOperator, BuiltinRelation, IrBuiltinOperators};
use crate::metadata::id_signature::{
    callable_signature, CallableShape, ClassScope, DeclarationContainer, KlibAccessorIdSignature,
    KlibDeclarationSignature, KlibPublicIdSignature, Placement,
};
use crate::metadata::semantic::KotlinType;
use crate::types::{Ty, TyKind};

use super::inline_ir::DeclarationSignatures;

/// What a body's nodes are translated against: the file, the identities its calls name, and the
/// declaring function's own parameters.
pub(super) struct BodyScope<'a> {
    pub(super) ir: &'a IrFile,
    pub(super) declarations: &'a DeclarationSignatures,
    pub(super) callables: &'a crate::backend::CheckedBackendCallables,
    pub(super) builtins: &'a IrBuiltinOperators,
    /// The declared type of each physical parameter slot; the dispatch receiver is slot zero.
    pub(super) parameters: &'a [Ty],
    pub(super) type_parameters: &'a [IrTypeParameter],
}

/// One expression with its Kotlin type.
#[derive(Clone, Debug)]
pub(super) struct BodyExpr {
    pub(super) ty: Ty,
    pub(super) kind: BodyKind,
}

/// The value a `GET_VAR` reads: a physical parameter slot, or a local variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ValueRef {
    Parameter(u32),
    Local(u32),
}

/// The callee a call names by exact signature.
#[derive(Clone, Debug)]
pub(super) enum CallTarget {
    Public(KlibPublicIdSignature),
    Accessor(KlibAccessorIdSignature),
}

/// Kotlin's `IrTypeOperator`, as it is numbered on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TypeOperator {
    Cast = 1,
    ImplicitCast = 2,
    ImplicitCoercionToUnit = 4,
    InstanceOf = 7,
    NotInstanceOf = 8,
}

#[derive(Clone, Debug)]
pub(super) enum BodyKind {
    Const(IrConst),
    GetValue(ValueRef),
    SetValue {
        local: u32,
        value: Box<BodyExpr>,
    },
    /// The value of the singleton class (`kotlin.Unit`).
    GetObject(crate::types::TypeName),
    Block {
        statements: Vec<BodyStatement>,
        origin: Option<&'static str>,
    },
    Return(Box<BodyExpr>),
    When {
        branches: Vec<(BodyExpr, BodyExpr)>,
        origin: Option<&'static str>,
    },
    TypeOperator {
        operator: TypeOperator,
        operand: Ty,
        argument: Box<BodyExpr>,
    },
    Call {
        target: CallTarget,
        type_arguments: Vec<Ty>,
        arguments: Vec<BodyExpr>,
        origin: Option<&'static str>,
    },
    StringConcat(Vec<BodyExpr>),
}

#[derive(Clone, Debug)]
pub(super) enum BodyStatement {
    Expression(BodyExpr),
    Variable(BodyVariable),
}

/// A local variable declaration, `IrVariable`.
#[derive(Clone, Debug)]
pub(super) struct BodyVariable {
    pub(super) local: u32,
    /// The source name; `None` for a compiler temporary.
    pub(super) name: Option<String>,
    pub(super) ty: Ty,
    pub(super) mutable: bool,
    pub(super) initializer: Option<BodyExpr>,
}

/// The statements of a function's block body, or `None` when a node is not translated yet.
pub(super) fn function_body(scope: &BodyScope<'_>, body: ExprId) -> Option<Vec<BodyStatement>> {
    // A source body is a block holding the written block; the outer one only groups it.
    let mut block = body;
    loop {
        let IrExpr::Block { stmts, value: None } = scope.ir.expr(block) else {
            return None;
        };
        match stmts.as_slice() {
            [only] if matches!(scope.ir.expr(*only), IrExpr::Block { value: None, .. }) => {
                block = *only;
            }
            _ => break,
        }
    }
    let IrExpr::Block { stmts, value: None } = scope.ir.expr(block) else {
        return None;
    };
    let mut translation = Translation {
        scope,
        locals: HashMap::default(),
        assigned: assigned_locals(scope.ir, block),
    };
    stmts
        .iter()
        .map(|statement| translation.statement(*statement))
        .collect()
}

/// Every local a `SET_VAR` in the body writes, which Kotlin declares `var`.
fn assigned_locals(ir: &IrFile, root: ExprId) -> HashSet<u32> {
    let mut assigned = HashSet::default();
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        if let IrExpr::SetValue { var, .. } = ir.expr(expression) {
            assigned.insert(*var);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assigned
}

struct Translation<'s, 'a> {
    scope: &'s BodyScope<'a>,
    /// The declared type of each local variable the body declared so far.
    locals: HashMap<u32, Ty>,
    assigned: HashSet<u32>,
}

impl Translation<'_, '_> {
    fn statement(&mut self, statement: ExprId) -> Option<BodyStatement> {
        if let IrExpr::Variable {
            index,
            ty,
            init,
            named,
        } = self.scope.ir.expr(statement)
        {
            if usize::try_from(*index).ok()? < self.scope.parameters.len()
                || self.locals.contains_key(index)
                || !self.supported(*ty)
            {
                return None;
            }
            let initializer = match init {
                Some(init) => Some(self.expr(*init)?),
                None => None,
            };
            let name = if *named {
                Some(self.scope.ir.value_names.get(&statement)?.clone())
            } else {
                None
            };
            self.locals.insert(*index, *ty);
            return Some(BodyStatement::Variable(BodyVariable {
                local: *index,
                name,
                ty: *ty,
                mutable: self.assigned.contains(index),
                initializer,
            }));
        }
        self.expr(statement).map(BodyStatement::Expression)
    }

    fn expr(&mut self, expression: ExprId) -> Option<BodyExpr> {
        let ir = self.scope.ir;
        let typed = |ty: Ty, kind: BodyKind| Some(BodyExpr { ty, kind });
        match ir.expr(expression) {
            IrExpr::Const(constant) => {
                typed(constant_type(constant)?, BodyKind::Const(constant.clone()))
            }
            IrExpr::GetValue(slot) => {
                let (value, ty) = self.value(*slot)?;
                typed(ty, BodyKind::GetValue(value))
            }
            IrExpr::SetValue { var, value } => {
                let declared = *self.locals.get(var)?;
                let value = self.expr(*value)?;
                if value.ty != declared && value.ty != Ty::Nothing {
                    return None;
                }
                typed(
                    Ty::Unit,
                    BodyKind::SetValue {
                        local: *var,
                        value: Box::new(value),
                    },
                )
            }
            IrExpr::Return(value) => {
                let value = match value {
                    Some(value) => self.expr(*value)?,
                    None => unit_value(),
                };
                typed(Ty::Nothing, BodyKind::Return(Box::new(value)))
            }
            IrExpr::Block { stmts, value } => {
                let mut statements = stmts
                    .iter()
                    .map(|statement| self.statement(*statement))
                    .collect::<Option<Vec<_>>>()?;
                let ty = match value {
                    // A statement block's `Unit` result is the block's own.
                    Some(value) if matches!(ir.expr(*value), IrExpr::UnitInstance) => Ty::Unit,
                    Some(value) => {
                        let value = self.expr(*value)?;
                        let ty = value.ty;
                        statements.push(BodyStatement::Expression(value));
                        ty
                    }
                    None => match statements.last() {
                        Some(BodyStatement::Expression(last)) if last.ty == Ty::Nothing => {
                            Ty::Nothing
                        }
                        _ => Ty::Unit,
                    },
                };
                typed(
                    ty,
                    BodyKind::Block {
                        statements,
                        origin: None,
                    },
                )
            }
            IrExpr::When { branches } => self.when(expression, branches),
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => self.type_operator(*op, *arg, *type_operand),
            IrExpr::Equality { op, mode, lhs, rhs } => self.equality(*op, Some(*mode), *lhs, *rhs),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(expression, *op, *lhs, *rhs),
            IrExpr::PrimitiveNeg { operand, ty } => {
                let operand = self.expr(*operand)?;
                if operand.ty != *ty {
                    return None;
                }
                let target = member_signature(*ty, "unaryMinus", None)?;
                typed(*ty, call(target, vec![operand], Some("UMINUS")))
            }
            IrExpr::NotNullAssert { operand, .. } => {
                let operand = self.expr(*operand)?;
                let ty = operand.ty.non_null();
                let target = self.builtin(BuiltinOperator::CheckNotNull)?;
                typed(
                    ty,
                    BodyKind::Call {
                        target,
                        type_arguments: vec![ty],
                        arguments: vec![operand],
                        origin: Some("EXCLEXCL"),
                    },
                )
            }
            IrExpr::StringConcat(parts) => {
                let parts = parts
                    .iter()
                    .map(|part| self.expr(*part))
                    .collect::<Option<Vec<_>>>()?;
                typed(Ty::String, BodyKind::StringConcat(parts))
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.call(expression, callee, *dispatch_receiver, args),
            IrExpr::Checked(operation) => self.checked(operation),
            IrExpr::UnitInstance => Some(unit_value()),
            _ => None,
        }
    }

    fn value(&self, slot: u32) -> Option<(ValueRef, Ty)> {
        match self.scope.parameters.get(usize::try_from(slot).ok()?) {
            Some(ty) => Some((ValueRef::Parameter(slot), *ty)),
            None => Some((ValueRef::Local(slot), *self.locals.get(&slot)?)),
        }
    }

    fn when(
        &mut self,
        expression: ExprId,
        branches: &[(Option<ExprId>, ExprId)],
    ) -> Option<BodyExpr> {
        let mut translated = Vec::with_capacity(branches.len());
        for (condition, result) in branches {
            let condition = match condition {
                Some(condition) => self.expr(*condition)?,
                None => boolean(true),
            };
            if condition.ty != Ty::Boolean {
                return None;
            }
            translated.push((condition, self.expr(*result)?));
        }
        let ty = match self.scope.ir.logical_type(expression) {
            Some(ty) => ty,
            None => {
                let (_, first) = translated.first()?;
                first.ty
            }
        };
        if !self.supported(ty) {
            return None;
        }
        Some(BodyExpr {
            ty,
            kind: BodyKind::When {
                branches: translated,
                origin: None,
            },
        })
    }

    fn type_operator(&mut self, op: IrTypeOp, argument: ExprId, operand: Ty) -> Option<BodyExpr> {
        if !self.supported(operand) {
            return None;
        }
        let argument = self.expr(argument)?;
        let (operator, ty) = match op {
            IrTypeOp::InstanceOf => (TypeOperator::InstanceOf, Ty::Boolean),
            IrTypeOp::NotInstanceOf => (TypeOperator::NotInstanceOf, Ty::Boolean),
            // A checked `as`, which throws on a value of another type.
            IrTypeOp::CastNonNull => (TypeOperator::Cast, operand),
            // A plain cast lets `null` pass: Kotlin's cast to the nullable target, or the smart
            // cast checking proved of a value that is not null.
            IrTypeOp::Cast if operand.is_nullable() => (TypeOperator::Cast, operand),
            IrTypeOp::Cast => (TypeOperator::ImplicitCast, operand),
            IrTypeOp::ImplicitCoercion if operand == Ty::Unit => {
                (TypeOperator::ImplicitCoercionToUnit, Ty::Unit)
            }
            // A coercion between two representations of one value; a conversion between two
            // primitive types is not one, and is not translated.
            IrTypeOp::ImplicitCoercion
                if argument.ty.non_null() == operand.non_null()
                    || !(argument.ty.non_null().is_numeric_or_char()
                        && operand.non_null().is_numeric_or_char()) =>
            {
                if argument.ty == operand {
                    return Some(argument);
                }
                (TypeOperator::ImplicitCast, operand)
            }
            _ => return None,
        };
        Some(BodyExpr {
            ty,
            kind: BodyKind::TypeOperator {
                operator,
                operand,
                argument: Box::new(argument),
            },
        })
    }

    /// `==`, `!=`, `===` or `!==`: kotlinc's `EQEQ`, `ieee754equals` or `EQEQEQ`, negated by
    /// `Boolean.not`.
    fn equality(
        &mut self,
        op: IrBinOp,
        mode: Option<EqualityMode>,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<BodyExpr> {
        let lhs = self.expr(lhs)?;
        let rhs = self.expr(rhs)?;
        let (operator, negated) = match op {
            IrBinOp::Eq | IrBinOp::Ne => {
                let operator = match mode {
                    Some(EqualityMode::Ieee754) => {
                        if lhs.ty != rhs.ty {
                            return None;
                        }
                        BuiltinOperator::Ieee754Equals { operand: lhs.ty }
                    }
                    _ => BuiltinOperator::Equals,
                };
                (operator, op == IrBinOp::Ne)
            }
            IrBinOp::RefEq | IrBinOp::RefNe => (BuiltinOperator::Identical, op == IrBinOp::RefNe),
            _ => return None,
        };
        let origin = match (operator, negated) {
            (BuiltinOperator::Identical, false) => "EQEQEQ",
            (BuiltinOperator::Identical, true) => "EXCLEQEQ",
            (_, false) => "EQEQ",
            (_, true) => "EXCLEQ",
        };
        let equal = BodyExpr {
            ty: Ty::Boolean,
            kind: call(self.builtin(operator)?, vec![lhs, rhs], Some(origin)),
        };
        if !negated {
            return Some(equal);
        }
        Some(BodyExpr {
            ty: Ty::Boolean,
            kind: call(
                self.builtin(BuiltinOperator::Not)?,
                vec![equal],
                Some(origin),
            ),
        })
    }

    fn binary(
        &mut self,
        expression: ExprId,
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<BodyExpr> {
        let ir = self.scope.ir;
        match op {
            IrBinOp::Eq | IrBinOp::Ne | IrBinOp::RefEq | IrBinOp::RefNe => {
                return self.equality(op, None, lhs, rhs);
            }
            IrBinOp::Lt | IrBinOp::Le | IrBinOp::Gt | IrBinOp::Ge => {
                // FIR's comparison `a < b` of two builtin scalars is the relation over `a` and `b`
                // itself, which common IR records as the compared result against zero.
                if let IrExpr::Call {
                    callee,
                    dispatch_receiver: Some(left),
                    args,
                } = ir.expr(lhs)
                {
                    if let Callee::Intrinsic {
                        operation:
                            IrIntrinsic::PrimitiveCompare {
                                operand,
                                relational_operator: true,
                            },
                        ..
                    } = &**callee
                    {
                        if matches!(ir.expr(rhs), IrExpr::Const(IrConst::Int(0))) {
                            let [right] = args.as_slice() else {
                                return None;
                            };
                            return self.relation(op, *operand, *left, *right);
                        }
                    }
                }
                let left = self.expr(lhs)?;
                return self.relation_of(op, left, rhs);
            }
            IrBinOp::And | IrBinOp::Or => return None,
            _ => {}
        }
        let left = self.expr(lhs)?;
        let right = self.expr(rhs)?;
        let name = match op {
            IrBinOp::Add => "plus",
            IrBinOp::Sub => "minus",
            IrBinOp::Mul => "times",
            IrBinOp::Div => "div",
            IrBinOp::Rem => "rem",
            IrBinOp::BitAnd => "and",
            IrBinOp::BitOr => "or",
            IrBinOp::BitXor => "xor",
            IrBinOp::Shl => "shl",
            IrBinOp::Shr => "shr",
            IrBinOp::Ushr => "ushr",
            _ => return None,
        };
        let origin = match op {
            IrBinOp::Add => Some("PLUS"),
            IrBinOp::Sub => Some("MINUS"),
            IrBinOp::Mul => Some("MUL"),
            IrBinOp::Div => Some("DIV"),
            IrBinOp::Rem => Some("PERC"),
            _ => None,
        };
        let ty = ir.logical_type(expression).unwrap_or(left.ty);
        if !ty.is_numeric_or_char() {
            return None;
        }
        let target = member_signature(left.ty, name, Some(right.ty))?;
        Some(BodyExpr {
            ty,
            kind: call(target, vec![left, right], origin),
        })
    }

    fn relation(
        &mut self,
        op: IrBinOp,
        operand: Ty,
        left: ExprId,
        right: ExprId,
    ) -> Option<BodyExpr> {
        let left = self.expr(left)?;
        let right = self.expr(right)?;
        if left.ty != operand || right.ty != operand {
            return None;
        }
        let target = self.builtin(BuiltinOperator::Relation(BuiltinRelation {
            operator: op,
            operand,
        }))?;
        Some(BodyExpr {
            ty: Ty::Boolean,
            kind: call(target, vec![left, right], Some(relation_origin(op))),
        })
    }

    /// A relation over an already translated left operand and `right`.
    fn relation_of(&mut self, op: IrBinOp, left: BodyExpr, right: ExprId) -> Option<BodyExpr> {
        let right = self.expr(right)?;
        if left.ty != right.ty {
            return None;
        }
        let target = self.builtin(BuiltinOperator::Relation(BuiltinRelation {
            operator: op,
            operand: left.ty,
        }))?;
        Some(BodyExpr {
            ty: Ty::Boolean,
            kind: call(target, vec![left, right], Some(relation_origin(op))),
        })
    }

    fn call(
        &mut self,
        expression: ExprId,
        callee: &Callee,
        dispatch_receiver: Option<ExprId>,
        args: &[ExprId],
    ) -> Option<BodyExpr> {
        let ir = self.scope.ir;
        let (target, ty) = match callee {
            Callee::Local(function) => {
                if dispatch_receiver.is_some() {
                    return None;
                }
                let (signature, None) = self.scope.declarations.get(*function)? else {
                    return None;
                };
                (
                    CallTarget::Public(signature.clone()),
                    ir.functions.get(*function as usize)?.ret,
                )
            }
            Callee::External {
                target,
                default_provider: None,
                params,
                ret,
                substitutions,
                defaults,
                extension_receiver_parameter: _,
            } if substitutions.is_empty() && defaults.is_empty() => {
                let fact = self.scope.callables.callable(*target)?;
                if params.len() != args.len() {
                    return None;
                }
                (call_target(fact)?, *ret)
            }
            Callee::Intrinsic {
                operation:
                    IrIntrinsic::PrimitiveCompare {
                        operand,
                        relational_operator: _,
                    },
                ret,
            } => {
                let [other] = args else {
                    return None;
                };
                let other = ir.logical_type(*other)?;
                (member_signature(*operand, "compareTo", Some(other))?, *ret)
            }
            _ => return None,
        };
        let ty = ir.logical_type(expression).unwrap_or(ty);
        if !self.supported(ty) {
            return None;
        }
        let arguments = dispatch_receiver
            .into_iter()
            .chain(args.iter().copied())
            .map(|argument| self.expr(argument))
            .collect::<Option<Vec<_>>>()?;
        Some(BodyExpr {
            ty,
            kind: BodyKind::Call {
                target,
                type_arguments: Vec::new(),
                arguments,
                origin: None,
            },
        })
    }

    fn checked(&mut self, operation: &IrCheckedOperation) -> Option<BodyExpr> {
        match operation {
            IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch: IrPropertyDispatch::Ordinary,
                receiver,
                arguments,
                parameters: _,
                result,
                substitutions,
                source_receiver: _,
            } if arguments.is_empty() && substitutions.is_empty() => {
                let property = self.scope.callables.property(*target)?;
                let getter = self.scope.callables.callable(property.getter)?;
                let target = call_target(getter)?;
                if !self.supported(*result) {
                    return None;
                }
                let arguments = receiver
                    .iter()
                    .map(|receiver| self.expr(*receiver))
                    .collect::<Option<Vec<_>>>()?;
                Some(BodyExpr {
                    ty: *result,
                    kind: BodyKind::Call {
                        target,
                        type_arguments: Vec::new(),
                        arguments,
                        origin: Some("GET_PROPERTY"),
                    },
                })
            }
            _ => None,
        }
    }

    fn builtin(&self, operator: BuiltinOperator) -> Option<CallTarget> {
        self.scope
            .builtins
            .signature(operator)
            .cloned()
            .map(CallTarget::Public)
    }

    fn supported(&self, ty: Ty) -> bool {
        supported_type(ty, self.scope.type_parameters)
    }
}

fn call(target: CallTarget, arguments: Vec<BodyExpr>, origin: Option<&'static str>) -> BodyKind {
    BodyKind::Call {
        target,
        type_arguments: Vec::new(),
        arguments,
        origin,
    }
}

fn boolean(value: bool) -> BodyExpr {
    BodyExpr {
        ty: Ty::Boolean,
        kind: BodyKind::Const(IrConst::Boolean(value)),
    }
}

/// `return` without a value returns `kotlin.Unit`'s object.
fn unit_value() -> BodyExpr {
    BodyExpr {
        ty: Ty::Unit,
        kind: BodyKind::GetObject(crate::types::TypeName::from("kotlin/Unit")),
    }
}

fn relation_origin(op: IrBinOp) -> &'static str {
    match op {
        IrBinOp::Lt => "LT",
        IrBinOp::Le => "LTEQ",
        IrBinOp::Gt => "GT",
        _ => "GTEQ",
    }
}

/// The KLIB identity a dependency callable's provider published for it.
fn call_target(fact: &crate::backend::BackendCallableFact) -> Option<CallTarget> {
    match &fact.dependency_callable.as_ref()?.declaration {
        KlibDeclarationSignature::Public(signature) => Some(CallTarget::Public(signature.clone())),
        KlibDeclarationSignature::Accessor(signature) => {
            Some(CallTarget::Accessor(signature.clone()))
        }
        KlibDeclarationSignature::FileLocal { .. } => None,
    }
}

/// The builtin scalar class's operator member `name` (`kotlin/Int.plus(kotlin.Int)`), which a
/// primitive operation of common IR realizes. Its identity is signed by the same mangler that
/// signs every library declaration.
fn member_signature(receiver: Ty, name: &str, parameter: Option<Ty>) -> Option<CallTarget> {
    if !receiver.is_numeric_or_char() || parameter.is_some_and(|ty| !ty.is_numeric_or_char()) {
        return None;
    }
    let class = receiver.obj_internal()?;
    if !class.package_matches("kotlin") {
        return None;
    }
    let package = ["kotlin".to_owned()];
    let classes = [ClassScope {
        name: class.segment_ref(),
        type_parameters: Vec::new(),
        expect: false,
    }];
    // Boundary conversion: the mangler signs the metadata class id.
    let parameter = match parameter {
        Some(ty) => Some(KotlinType::class(ty.obj_internal()?.render())),
        None => None,
    };
    let signature = callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &classes,
            native_interop_library: false,
        },
        &CallableShape {
            name,
            contexts: Vec::new(),
            receiver: None,
            params: parameter.iter().collect(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .ok()?;
    Some(CallTarget::Public(signature))
}

fn constant_type(constant: &IrConst) -> Option<Ty> {
    Some(match constant {
        IrConst::Null => Ty::nullable(Ty::Nothing),
        IrConst::Boolean(_) => Ty::Boolean,
        IrConst::Int(_) => Ty::Int,
        IrConst::Long(_) => Ty::Long,
        IrConst::Double(_) => Ty::Double,
        // A string with a lone surrogate has no UTF-8 spelling in the string table.
        IrConst::String(value) if value.as_str().is_some() => Ty::String,
        _ => return None,
    })
}

/// Whether the type tables can write `ty`.
pub(super) fn supported_type(ty: Ty, parameters: &[IrTypeParameter]) -> bool {
    match ty.kind() {
        TyKind::Unit | TyKind::Nothing | TyKind::BOOLEAN => true,
        TyKind::Obj(_, arguments) => arguments.iter().all(|argument| match argument.kind() {
            TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner) => supported_type(inner, parameters),
            _ => supported_type(*argument, parameters),
        }),
        TyKind::Nullable(inner) => supported_type(inner, parameters),
        TyKind::InProjection(_)
        | TyKind::OutProjection(_)
        | TyKind::StarProjection(_)
        | TyKind::RawProjection(_) => false,
        TyKind::TyParam(..) => parameters.iter().any(|parameter| parameter.ty() == ty),
        TyKind::Null
        | TyKind::Error
        | TyKind::Pending
        | TyKind::Var(..)
        | TyKind::Fun(_)
        | TyKind::DefinitelyNotNull(_)
        | TyKind::PlatformNullable(_)
        | TyKind::MutabilityFlexible(_)
        | TyKind::Intersection(_) => false,
    }
}
