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
    /// The identities this file publishes for its classes, constructors and properties.
    pub(super) exports: &'a crate::backend::DependencyExportSignatures,
    /// The declared type of each physical parameter slot; the dispatch receiver is slot zero.
    pub(super) parameters: &'a [Ty],
    pub(super) type_parameters: &'a [IrTypeParameter],
}

/// An expression of a [`FunctionBody`], by its index in the body's expression arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BodyExprId(u32);

/// One expression with its Kotlin type.
#[derive(Clone, Debug)]
pub(super) struct BodyExpr {
    pub(super) ty: Ty,
    pub(super) kind: BodyKind,
}

/// A function's block body: its statements, over the arena that holds every expression they
/// reach. A node names its operands by [`BodyExprId`].
#[derive(Clone, Debug, Default)]
pub(super) struct FunctionBody {
    exprs: Vec<BodyExpr>,
    pub(super) statements: Vec<BodyStatement>,
}

impl FunctionBody {
    /// Adds an expression whose operands are already in the arena.
    pub(super) fn push(&mut self, ty: Ty, kind: BodyKind) -> BodyExprId {
        let id = BodyExprId(u32::try_from(self.exprs.len()).expect("a body has under 2^32 nodes"));
        self.exprs.push(BodyExpr { ty, kind });
        id
    }

    pub(super) fn expr(&self, id: BodyExprId) -> &BodyExpr {
        &self.exprs[id.0 as usize]
    }

    fn ty(&self, id: BodyExprId) -> Ty {
        self.expr(id).ty
    }
}

/// The value a `GET_VAR` reads: a physical parameter slot, or a local variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ValueRef {
    Parameter(u32),
    Local(u32),
}

/// The callee a call names by exact signature.
#[derive(Clone, Debug, PartialEq)]
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
        value: BodyExprId,
    },
    /// The value of the singleton class (`kotlin.Unit`).
    GetObject(crate::types::TypeName),
    Block {
        statements: Vec<BodyStatement>,
        origin: Option<&'static str>,
    },
    Return(BodyExprId),
    When {
        branches: Vec<(BodyExprId, BodyExprId)>,
        origin: Option<&'static str>,
    },
    TypeOperator {
        operator: TypeOperator,
        operand: Ty,
        argument: BodyExprId,
    },
    Call {
        target: CallTarget,
        type_arguments: Vec<Ty>,
        arguments: Vec<BodyExprId>,
        origin: Option<&'static str>,
    },
    StringConcat(Vec<BodyExprId>),
    /// `CONSTRUCTOR_CALL`: a new instance of the constructed class, `type_arguments` holding the
    /// class's type arguments and then the constructor's own `constructor_type_arguments`.
    ConstructorCall {
        target: CallTarget,
        constructor_type_arguments: u32,
        type_arguments: Vec<Ty>,
        arguments: Vec<BodyExprId>,
        origin: Option<&'static str>,
    },
    /// A constructor's call of its superclass's or its own class's other constructor.
    DelegatingConstructorCall {
        target: CallTarget,
        type_arguments: Vec<Ty>,
        arguments: Vec<BodyExprId>,
    },
    /// A primary constructor's run of its class's property initializers and `init` blocks.
    InstanceInitializerCall(crate::types::TypeName),
    /// A read of the backing field of the property `property` signs.
    GetField {
        property: KlibPublicIdSignature,
        receiver: Option<BodyExprId>,
    },
    /// A write of the backing field of the property `property` signs.
    SetField {
        property: KlibPublicIdSignature,
        receiver: Option<BodyExprId>,
        value: BodyExprId,
    },
}

#[derive(Clone, Debug)]
pub(super) enum BodyStatement {
    Expression(BodyExprId),
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
    pub(super) initializer: Option<BodyExprId>,
}

/// The block body of a function, or `None` when a node is not translated yet.
pub(super) fn function_body(scope: &BodyScope<'_>, body: ExprId) -> Option<FunctionBody> {
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
        body: FunctionBody::default(),
        locals: HashMap::default(),
        assigned: assigned_locals(scope.ir, block),
    };
    let statements = stmts
        .iter()
        .map(|statement| translation.statement(*statement))
        .collect::<Option<Vec<_>>>()?;
    let mut body = translation.body;
    body.statements = statements;
    Some(body)
}

/// The single expression `expression`, such as a property's initializer, with the body that
/// holds its nodes. `None` when a node is not translated yet.
pub(super) fn expression_body(
    scope: &BodyScope<'_>,
    expression: ExprId,
) -> Option<(FunctionBody, BodyExprId)> {
    let mut translation = Translation {
        scope,
        body: FunctionBody::default(),
        locals: HashMap::default(),
        assigned: assigned_locals(scope.ir, expression),
    };
    let root = translation.expr(expression)?;
    Some((translation.body, root))
}

/// A constructor's block body: its delegation to `target` with `arguments`, then, for a primary
/// constructor, the run of `initialized`'s property initializers. `None` when an argument is not
/// translated yet.
pub(super) fn constructor_body(
    scope: &BodyScope<'_>,
    target: CallTarget,
    arguments: &[ExprId],
    initialized: Option<crate::types::TypeName>,
) -> Option<FunctionBody> {
    let mut translation = Translation {
        scope,
        body: FunctionBody::default(),
        locals: HashMap::default(),
        assigned: HashSet::default(),
    };
    let arguments = arguments
        .iter()
        .map(|argument| translation.expr(*argument))
        .collect::<Option<Vec<_>>>()?;
    let mut body = translation.body;
    let delegation = body.push(
        Ty::Unit,
        BodyKind::DelegatingConstructorCall {
            target,
            type_arguments: Vec::new(),
            arguments,
        },
    );
    body.statements.push(BodyStatement::Expression(delegation));
    if let Some(class) = initialized {
        let initializer = body.push(Ty::Unit, BodyKind::InstanceInitializerCall(class));
        body.statements.push(BodyStatement::Expression(initializer));
    }
    Some(body)
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
    body: FunctionBody,
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

    fn push(&mut self, ty: Ty, kind: BodyKind) -> Option<BodyExprId> {
        Some(self.body.push(ty, kind))
    }

    fn ty(&self, expression: BodyExprId) -> Ty {
        self.body.ty(expression)
    }

    fn expr(&mut self, expression: ExprId) -> Option<BodyExprId> {
        let ir = self.scope.ir;
        match ir.expr(expression) {
            IrExpr::Const(constant) => {
                self.push(constant_type(constant)?, BodyKind::Const(constant.clone()))
            }
            IrExpr::GetValue(slot) => {
                let (value, ty) = self.value(*slot)?;
                self.push(ty, BodyKind::GetValue(value))
            }
            IrExpr::SetValue { var, value } => {
                let declared = *self.locals.get(var)?;
                let value = self.expr(*value)?;
                if self.ty(value) != declared && self.ty(value) != Ty::Nothing {
                    return None;
                }
                self.push(Ty::Unit, BodyKind::SetValue { local: *var, value })
            }
            IrExpr::Return(value) => {
                let value = match value {
                    Some(value) => self.expr(*value)?,
                    None => self.unit_value(),
                };
                self.push(Ty::Nothing, BodyKind::Return(value))
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
                        statements.push(BodyStatement::Expression(value));
                        self.ty(value)
                    }
                    None => match statements.last() {
                        Some(BodyStatement::Expression(last)) if self.ty(*last) == Ty::Nothing => {
                            Ty::Nothing
                        }
                        _ => Ty::Unit,
                    },
                };
                self.push(
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
                ..
            } => self.type_operator(*op, *arg, *type_operand),
            IrExpr::Equality { op, mode, lhs, rhs } => self.equality(*op, Some(*mode), *lhs, *rhs),
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => self.binary(expression, *op, *lhs, *rhs),
            IrExpr::PrimitiveNeg { operand, ty } => {
                let operand = self.expr(*operand)?;
                if self.ty(operand) != *ty {
                    return None;
                }
                let target = member_signature(*ty, "unaryMinus", None)?;
                self.push(*ty, call(target, vec![operand], Some("UMINUS")))
            }
            IrExpr::NotNullAssert { operand, .. } => {
                let operand = self.expr(*operand)?;
                let ty = self.ty(operand).non_null();
                let target = self.builtin(BuiltinOperator::CheckNotNull)?;
                self.push(
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
                self.push(Ty::String, BodyKind::StringConcat(parts))
            }
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.call(expression, callee, *dispatch_receiver, args),
            // A virtual call of a member this file's class declares, with every argument given.
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } => {
                let function = *ir
                    .classes
                    .get(*class as usize)?
                    .methods
                    .get(*index as usize)?;
                let args = args.iter().copied().collect::<Option<Vec<_>>>()?;
                self.call(expression, &Callee::Local(function), Some(*receiver), &args)
            }
            IrExpr::Checked(operation) => self.checked(expression, operation),
            IrExpr::New {
                internal,
                args,
                defaults,
                default_prefix_count: 0,
                external_target: None,
                ..
            } if defaults.is_empty() => self.construction(expression, *internal, args),
            IrExpr::UnitInstance => Some(self.unit_value()),
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
    ) -> Option<BodyExprId> {
        let mut translated = Vec::with_capacity(branches.len());
        for (condition, result) in branches {
            let condition = match condition {
                Some(condition) => self.expr(*condition)?,
                None => self
                    .body
                    .push(Ty::Boolean, BodyKind::Const(IrConst::Boolean(true))),
            };
            if self.ty(condition) != Ty::Boolean {
                return None;
            }
            let result = self.expr(*result)?;
            translated.push((condition, result));
        }
        let ty = match self.scope.ir.logical_type(expression) {
            Some(ty) => ty,
            None => {
                let (_, first) = translated.first()?;
                self.ty(*first)
            }
        };
        if !self.supported(ty) {
            return None;
        }
        self.push(
            ty,
            BodyKind::When {
                branches: translated,
                origin: None,
            },
        )
    }

    fn type_operator(&mut self, op: IrTypeOp, argument: ExprId, operand: Ty) -> Option<BodyExprId> {
        if !self.supported(operand) {
            return None;
        }
        let argument = self.expr(argument)?;
        let argument_ty = self.ty(argument);
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
                if argument_ty.non_null() == operand.non_null()
                    || !(argument_ty.non_null().is_numeric_or_char()
                        && operand.non_null().is_numeric_or_char()) =>
            {
                if argument_ty == operand {
                    return Some(argument);
                }
                (TypeOperator::ImplicitCast, operand)
            }
            _ => return None,
        };
        self.push(
            ty,
            BodyKind::TypeOperator {
                operator,
                operand,
                argument,
            },
        )
    }

    /// `==`, `!=`, `===` or `!==`: kotlinc's `EQEQ`, `ieee754equals` or `EQEQEQ`, negated by
    /// `Boolean.not`.
    fn equality(
        &mut self,
        op: IrBinOp,
        mode: Option<EqualityMode>,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<BodyExprId> {
        let lhs = self.expr(lhs)?;
        let rhs = self.expr(rhs)?;
        let (operator, negated) = match op {
            IrBinOp::Eq | IrBinOp::Ne => {
                let operator = match mode {
                    Some(EqualityMode::Ieee754) => {
                        if self.ty(lhs) != self.ty(rhs) {
                            return None;
                        }
                        BuiltinOperator::Ieee754Equals {
                            operand: self.ty(lhs),
                        }
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
        let target = self.builtin(operator)?;
        let equal = self.push(Ty::Boolean, call(target, vec![lhs, rhs], Some(origin)))?;
        if !negated {
            return Some(equal);
        }
        let not = self.builtin(BuiltinOperator::Not)?;
        self.push(Ty::Boolean, call(not, vec![equal], Some(origin)))
    }

    fn binary(
        &mut self,
        expression: ExprId,
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<BodyExprId> {
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
                            let left = self.expr(*left)?;
                            let right = self.expr(*right)?;
                            if self.ty(left) != *operand {
                                return None;
                            }
                            return self.relation(op, left, right);
                        }
                    }
                }
                let left = self.expr(lhs)?;
                let right = self.expr(rhs)?;
                return self.relation(op, left, right);
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
        let ty = ir.logical_type(expression).unwrap_or(self.ty(left));
        if !ty.is_numeric_or_char() {
            return None;
        }
        let target = member_signature(self.ty(left), name, Some(self.ty(right)))?;
        self.push(ty, call(target, vec![left, right], origin))
    }

    /// The builtin relation `op` over two operands of one type.
    fn relation(&mut self, op: IrBinOp, left: BodyExprId, right: BodyExprId) -> Option<BodyExprId> {
        let operand = self.ty(left);
        if self.ty(right) != operand {
            return None;
        }
        let target = self.builtin(BuiltinOperator::Relation(BuiltinRelation {
            operator: op,
            operand,
        }))?;
        self.push(
            Ty::Boolean,
            call(target, vec![left, right], Some(relation_origin(op))),
        )
    }

    fn call(
        &mut self,
        expression: ExprId,
        callee: &Callee,
        dispatch_receiver: Option<ExprId>,
        args: &[ExprId],
    ) -> Option<BodyExprId> {
        let ir = self.scope.ir;
        let mut local = false;
        let (target, ty) = match callee {
            Callee::Local(function) => {
                local = true;
                // A call of a generic function passes its type arguments, which common IR does not
                // record on a local call yet.
                if ir
                    .signatures
                    .get(function)
                    .is_some_and(|generic| !generic.type_params.is_empty())
                {
                    return None;
                }
                let (signature, receiver) = self.scope.declarations.get(*function)?;
                // A member takes its dispatch receiver, and a top-level function takes none.
                if receiver.is_some() != dispatch_receiver.is_some() {
                    return None;
                }
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
                if params.len() != args.len() || generic(fact) {
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
        let target = match (local, dispatch_receiver) {
            (true, Some(_)) => self.received_on(target, arguments.first().copied())?,
            _ => target,
        };
        self.push(ty, call(target, arguments, None))
    }

    /// The member `target` of this file's classes as `receiver`'s class names it. A member a
    /// subclass inherits is called as that subclass's own (fake-override) member, which keeps the
    /// declaration's member ids under the subclass's paths, as kotlinc writes it. `None` when the
    /// receiver's class belongs to another module, whose identity this writer does not name.
    fn received_on(&self, target: CallTarget, receiver: Option<BodyExprId>) -> Option<CallTarget> {
        let Some(receiver) = receiver else {
            return Some(target);
        };
        let TyKind::Obj(class, arguments) = self.ty(receiver).kind() else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let class = &self
            .scope
            .exports
            .classifier(self.scope.ir.class_id_by_name(class)?)?
            .signature;
        let owner = match &target {
            CallTarget::Public(signature) => signature,
            CallTarget::Accessor(signature) => signature.property(),
        };
        let declared_here = owner.package() == class.package()
            && owner.declaration().segments().split_last()?.1 == class.declaration().segments();
        if declared_here {
            return Some(target);
        }
        Some(match target {
            CallTarget::Public(signature) => CallTarget::Public(signature.as_member_of(class)?),
            CallTarget::Accessor(signature) => CallTarget::Accessor(signature.as_member_of(class)?),
        })
    }

    fn checked(
        &mut self,
        expression: ExprId,
        operation: &IrCheckedOperation,
    ) -> Option<BodyExprId> {
        match operation {
            IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver: None,
                context_arguments,
                substitutions,
            } if context_arguments.is_empty() && substitutions.is_empty() => {
                let ty = self.scope.ir.logical_type(expression)?;
                if !self.supported(ty) {
                    return None;
                }
                let getter = self.property_accessor(*target, false)?;
                let arguments = dispatch_receiver
                    .iter()
                    .map(|receiver| self.expr(*receiver))
                    .collect::<Option<Vec<_>>>()?;
                let getter = self.received_on(getter, arguments.first().copied())?;
                self.push(ty, call(getter, arguments, Some("GET_PROPERTY")))
            }
            IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver: None,
                context_arguments,
                value,
                substitutions,
            } if context_arguments.is_empty() && substitutions.is_empty() => {
                let setter = self.property_accessor(*target, true)?;
                let mut arguments = dispatch_receiver
                    .iter()
                    .map(|receiver| self.expr(*receiver))
                    .collect::<Option<Vec<_>>>()?;
                let setter = self.received_on(setter, arguments.first().copied())?;
                arguments.push(self.expr(*value)?);
                self.push(Ty::Unit, call(setter, arguments, Some("EQ")))
            }
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
                if generic(getter) {
                    return None;
                }
                let target = call_target(getter)?;
                if !self.supported(*result) {
                    return None;
                }
                let arguments = receiver
                    .iter()
                    .map(|receiver| self.expr(*receiver))
                    .collect::<Option<Vec<_>>>()?;
                self.push(*result, call(target, arguments, Some("GET_PROPERTY")))
            }
            _ => None,
        }
    }

    /// The identity of a property of this file's getter, or its setter.
    fn property_accessor(
        &self,
        property: crate::fir::PropertyId,
        setter: bool,
    ) -> Option<CallTarget> {
        property_accessor(self.scope.ir, self.scope.exports, property, setter)
    }

    /// `C(args)` of a class of this file, by the constructor checking selected.
    fn construction(
        &mut self,
        expression: ExprId,
        class: crate::types::TypeName,
        args: &[ExprId],
    ) -> Option<BodyExprId> {
        let ir = self.scope.ir;
        let ty = ir.logical_type(expression)?;
        // A generic class passes type arguments, which this writer does not record yet.
        if !matches!(ty.kind(), TyKind::Obj(name, arguments) if name == class && arguments.is_empty())
        {
            return None;
        }
        let target = constructor_target(
            self.scope,
            class,
            *ir.construction_targets.get(&expression)?,
        )?;
        let arguments = args
            .iter()
            .map(|argument| self.expr(*argument))
            .collect::<Option<Vec<_>>>()?;
        self.push(
            ty,
            BodyKind::ConstructorCall {
                target,
                constructor_type_arguments: 0,
                type_arguments: Vec::new(),
                arguments,
                origin: None,
            },
        )
    }

    /// `return` without a value returns `kotlin.Unit`'s object.
    fn unit_value(&mut self) -> BodyExprId {
        self.body.push(
            Ty::Unit,
            BodyKind::GetObject(crate::types::TypeName::from("kotlin/Unit")),
        )
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

/// The identity this file publishes for the constructor `target` of its class `class`.
/// The identity of a property of this file's getter, or its setter.
pub(super) fn property_accessor(
    ir: &IrFile,
    exports: &crate::backend::DependencyExportSignatures,
    property: crate::fir::PropertyId,
    setter: bool,
) -> Option<CallTarget> {
    // A written accessor is the function its body was checked as.
    let written = match ir.local_property_layouts.get(&property)? {
        crate::ir::IrLocalPropertyLayout::Member {
            getter,
            setter: written_setter,
            ..
        }
        | crate::ir::IrLocalPropertyLayout::TopLevelStorage {
            getter,
            setter: written_setter,
            ..
        } => {
            if setter {
                *written_setter
            } else {
                *getter
            }
        }
        _ => return None,
    };
    let identity = match written {
        Some(function) => exports.callable(function)?,
        None => {
            let operation = if setter {
                crate::backend::ExportedPropertyAccessor::Setter(property)
            } else {
                crate::backend::ExportedPropertyAccessor::Getter(property)
            };
            &exports
                .property_accessors()
                .iter()
                .find(|(exported, _)| *exported == operation)?
                .1
        }
    };
    match &identity.declaration {
        KlibDeclarationSignature::Accessor(signature) => {
            Some(CallTarget::Accessor(signature.clone()))
        }
        _ => None,
    }
}

pub(super) fn constructor_target(
    scope: &BodyScope<'_>,
    class: crate::types::TypeName,
    target: crate::ir::IrConstructorTarget,
) -> Option<CallTarget> {
    let class = scope.ir.class_id_by_name(class)?;
    let secondary = match target.ordinal {
        0 => None,
        ordinal => Some(usize::try_from(ordinal - 1).ok()?),
    };
    let wanted = crate::backend::ExportedConstructor { class, secondary };
    let exports = scope.exports;
    let (_, identity) = exports
        .constructors()
        .iter()
        .chain(exports.constructor_declarations())
        .find(|(constructor, _)| *constructor == wanted)?;
    match &identity.declaration {
        KlibDeclarationSignature::Public(signature) => Some(CallTarget::Public(signature.clone())),
        _ => None,
    }
}

fn call(target: CallTarget, arguments: Vec<BodyExprId>, origin: Option<&'static str>) -> BodyKind {
    BodyKind::Call {
        target,
        type_arguments: Vec::new(),
        arguments,
        origin,
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

/// Whether a dependency callable declares type parameters, whose arguments a call of it passes.
fn generic(fact: &crate::backend::BackendCallableFact) -> bool {
    fact.generic_sig
        .as_ref()
        .is_some_and(|signature| !signature.formals.is_empty())
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
