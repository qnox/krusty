//! A decoded KLIB body as checked common IR.
//!
//! Each modelled KLIB IR form lowers to exactly the common IR checked FIR lowering produces for the
//! equivalent source, with the same per-expression facts (logical types, return depths, binding
//! stability, `when` facts), so a backend compiles a dependency body as it compiles a module one.
//! The match over KLIB expression forms is exhaustive: every form either has a lowering here or
//! declines by its name.

use super::decline::KlibBodyDeclineReason;
use super::function_lowering::FunctionHeader;
use super::ir_builtins::IrBuiltinOperators;
use super::klib_types::semantic_type;
use crate::ir::{complete_bottom_value, ExprId, IrBindingStability, IrConst, IrExpr, IrFile};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBranch, KlibIrExpr, KlibIrExprId, KlibIrExprKind,
    KlibIrMemberAccess, KlibIrStatement, KlibIrTypeOperator,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSignature, KlibIrSymbol};
use crate::types::Ty;

type Lowered<T> = Result<T, KlibBodyDeclineReason>;

/// Lowers the body of one function into `ir`.
pub(super) struct BodyLowering<'a> {
    pub(super) arena: &'a KlibIrArena,
    /// The function whose body this is: the only target its `return`s may name.
    pub(super) function: &'a KlibIrSymbol,
    pub(super) header: &'a FunctionHeader,
    pub(super) builtins: &'a IrBuiltinOperators,
    pub(super) ir: &'a mut IrFile,
}

impl BodyLowering<'_> {
    /// The function's body. Checked FIR lowering lowers a block body as the source block, typed
    /// by its checked type, inside the callable's own block; both are the callable's scope. A body
    /// that can reach its end without a `return` declines: that is an implicit `Unit` result this
    /// lowering does not model. One that cannot is typed `Nothing`.
    pub(super) fn body(mut self, statements: &[KlibIrStatement]) -> Lowered<ExprId> {
        let returns = statements.last().is_some_and(|statement| {
            matches!(
                statement,
                KlibIrStatement::Expression(expression)
                    if matches!(self.arena.expr(*expression).kind, KlibIrExprKind::Return { .. })
            )
        });
        if !returns {
            return Err(KlibBodyDeclineReason::MissingReturn);
        }
        let stmts = statements
            .iter()
            .map(|statement| self.statement(statement))
            .collect::<Lowered<Vec<_>>>()?;
        let source = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        self.ir.logical_types.insert(source, Ty::Nothing);
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![source],
            value: None,
        });
        self.ir.callable_scopes.insert(source);
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    fn statement(&mut self, statement: &KlibIrStatement) -> Lowered<ExprId> {
        match statement {
            KlibIrStatement::Expression(expression) => self.expression(*expression),
            KlibIrStatement::Variable(_) => unsupported("a local variable"),
            KlibIrStatement::Function(_) => unsupported("a local function"),
            KlibIrStatement::Class(_) => unsupported("a local class"),
            KlibIrStatement::LocalDelegatedProperty(_) => unsupported("a local delegated property"),
            KlibIrStatement::TypeAlias(_) => unsupported("a local type alias"),
        }
    }

    fn expression(&mut self, id: KlibIrExprId) -> Lowered<ExprId> {
        let arena = self.arena;
        let expression = arena.expr(id);
        let (lowered, ty) = match &expression.kind {
            KlibIrExprKind::Const(constant) => {
                let ty = self.ty(expression, "constant")?;
                (self.constant(constant, ty)?, ty)
            }
            KlibIrExprKind::GetValue { symbol, origin: _ } => {
                let ty = self.ty(expression, "value read")?;
                (self.read(symbol)?, ty)
            }
            KlibIrExprKind::Call {
                access,
                super_qualifier,
            } => {
                let ty = self.ty(expression, "call")?;
                if super_qualifier.is_some() {
                    return unsupported("a `super` call");
                }
                (self.call(access, ty)?, ty)
            }
            KlibIrExprKind::Return { target, value } => {
                let ty = self.ty(expression, "`return`")?;
                (self.return_value(target, *value)?, ty)
            }
            KlibIrExprKind::When {
                branches,
                origin: _,
            } => {
                let ty = self.ty(expression, "`when`")?;
                (self.conditional(branches, ty)?, ty)
            }
            KlibIrExprKind::SetValue { .. } => return unsupported("an assignment"),
            KlibIrExprKind::ConstructorCall { .. } => return unsupported("a constructor call"),
            KlibIrExprKind::DelegatingConstructorCall(_) => {
                return unsupported("a delegating constructor call")
            }
            KlibIrExprKind::EnumConstructorCall(_) => {
                return unsupported("an enum constructor call")
            }
            KlibIrExprKind::FunctionReference { .. }
            | KlibIrExprKind::RichFunctionReference { .. } => {
                return unsupported("a function reference")
            }
            KlibIrExprKind::PropertyReference { .. }
            | KlibIrExprKind::RichPropertyReference { .. } => {
                return unsupported("a property reference")
            }
            KlibIrExprKind::LocalDelegatedPropertyReference { .. } => {
                return unsupported("a local delegated property reference")
            }
            KlibIrExprKind::Block { .. } => return unsupported("a block"),
            KlibIrExprKind::Composite { .. } => return unsupported("a composite block"),
            KlibIrExprKind::ReturnableBlock { .. } => return unsupported("a returnable block"),
            KlibIrExprKind::InlinedFunctionBlock { .. } => {
                return unsupported("an inlined function block")
            }
            KlibIrExprKind::TypeOperator { operator, .. } => {
                return unsupported(type_operator_name(*operator))
            }
            KlibIrExprKind::GetField { .. } => return unsupported("a field read"),
            KlibIrExprKind::SetField { .. } => return unsupported("a field write"),
            KlibIrExprKind::GetObject(_) => return unsupported("an object value"),
            KlibIrExprKind::GetClass(_) => return unsupported("a class literal of a value"),
            KlibIrExprKind::ClassReference { .. } => return unsupported("a class literal"),
            KlibIrExprKind::GetEnumValue(_) => return unsupported("an enum entry"),
            KlibIrExprKind::Break { .. } => return unsupported("`break`"),
            KlibIrExprKind::Continue { .. } => return unsupported("`continue`"),
            KlibIrExprKind::While(_) => return unsupported("a `while` loop"),
            KlibIrExprKind::DoWhile(_) => return unsupported("a `do`-`while` loop"),
            KlibIrExprKind::InstanceInitializerCall(_) => {
                return unsupported("an instance initializer call")
            }
            KlibIrExprKind::StringConcat(_) => return unsupported("a string template"),
            KlibIrExprKind::Throw(_) => return unsupported("`throw`"),
            KlibIrExprKind::Try { .. } => return unsupported("`try`"),
            KlibIrExprKind::Vararg { .. } => return unsupported("a vararg argument"),
            KlibIrExprKind::FunctionExpression { .. } => return unsupported("a lambda"),
            KlibIrExprKind::DynamicMember { .. } => return unsupported("a dynamic member access"),
            KlibIrExprKind::DynamicOperator { .. } => return unsupported("a dynamic operator"),
            KlibIrExprKind::Error { .. } => return unsupported("an error expression"),
            KlibIrExprKind::ErrorCall { .. } => return unsupported("an error call"),
            KlibIrExprKind::Missing => return unsupported("an omitted argument"),
        };
        // Checked FIR lowering records the checked type of every expression it lowers, around the
        // bottom-value completion it attaches to a `Nothing` producer.
        self.ir.logical_types.insert(lowered, ty);
        let completed = complete_bottom_value(self.ir, lowered, ty);
        self.ir.logical_types.insert(completed, ty);
        Ok(completed)
    }

    /// The type serialized for `expression`, a `form`.
    fn ty(&self, expression: &KlibIrExpr, form: &'static str) -> Lowered<Ty> {
        let ty = expression
            .ty
            .ok_or(KlibBodyDeclineReason::UntypedExpression(form))?;
        semantic_type(self.arena, ty)
    }

    /// Lower `id` as a value used where `expected` is: the serialized body states every conversion
    /// explicitly, and this lowering models none.
    fn operand(&mut self, id: KlibIrExprId, expected: Ty) -> Lowered<ExprId> {
        let lowered = self.expression(id)?;
        if self.ir.logical_types.get(&lowered) != Some(&expected) {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        Ok(lowered)
    }

    fn constant(&mut self, constant: &KlibIrConstant, ty: Ty) -> Lowered<ExprId> {
        let (value, constant_ty) = match constant {
            KlibIrConstant::Null => (IrConst::Null, None),
            KlibIrConstant::Boolean(value) => (IrConst::Boolean(*value), Some(Ty::Boolean)),
            KlibIrConstant::Char(value) => (IrConst::Char(*value), Some(Ty::Char)),
            KlibIrConstant::Byte(value) => (IrConst::Byte(*value), Some(Ty::Byte)),
            KlibIrConstant::Short(value) => (IrConst::Short(*value), Some(Ty::Short)),
            KlibIrConstant::Int(value) => (IrConst::Int(*value), Some(Ty::Int)),
            KlibIrConstant::Long(value) => (IrConst::Long(*value), Some(Ty::Long)),
            KlibIrConstant::Float(value) => (IrConst::Float(*value), Some(Ty::Float)),
            KlibIrConstant::Double(value) => (IrConst::Double(*value), Some(Ty::Double)),
            KlibIrConstant::String(value) => {
                (IrConst::String(value.as_str().into()), Some(Ty::String))
            }
        };
        let matches = match constant_ty {
            Some(constant_ty) => constant_ty == ty,
            None => ty.is_nullable(),
        };
        if !matches {
            // An unsigned constant, for one, is serialized as its signed value typed `UInt`.
            return unsupported("a constant of another type than its value's");
        }
        Ok(self.ir.add_expr(IrExpr::Const(value)))
    }

    fn read(&mut self, symbol: &KlibIrSymbol) -> Lowered<ExprId> {
        let value = self
            .header
            .value(symbol)
            .ok_or(KlibBodyDeclineReason::ForeignValue)?;
        let read = self.ir.add_expr(IrExpr::GetValue(value.index));
        if value.stable_read {
            self.ir
                .binding_read_stability
                .insert(read, IrBindingStability::Stable);
        }
        Ok(read)
    }

    /// A call of a compiler built-in relation, lowered as checked FIR lowers the source operator
    /// over two operands of one primitive type: the primitive relation itself. On `Float` and
    /// `Double` that relation is the IEEE one, as the built-in is.
    fn call(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        let KlibIrSignature::Public(callee) = &access.symbol.signature else {
            return unsupported("a call of a file-private declaration");
        };
        let relation = self
            .builtins
            .relation(callee)
            .ok_or_else(|| KlibBodyDeclineReason::UnsupportedCallee(Box::new(callee.clone())))?;
        let operands = match &access.arguments {
            KlibIrArguments::Flat(arguments) => arguments.as_slice(),
            KlibIrArguments::Split {
                dispatch_receiver: None,
                extension_receiver: None,
                values,
            } => values.as_slice(),
            KlibIrArguments::Split { .. } => &[],
        };
        let (&[Some(lhs), Some(rhs)], true, true) = (
            operands,
            access.type_arguments.is_empty(),
            ty == Ty::Boolean,
        ) else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a built-in relation is called with another shape than its declaration's"
                    .to_owned(),
            ));
        };
        let lhs = self.operand(lhs, relation.operand)?;
        let rhs = self.operand(rhs, relation.operand)?;
        Ok(self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: relation.operator,
            lhs,
            rhs,
        }))
    }

    fn return_value(&mut self, target: &KlibIrSymbol, value: KlibIrExprId) -> Lowered<ExprId> {
        if target != self.function {
            return Err(KlibBodyDeclineReason::ForeignReturnTarget);
        }
        let value = self.operand(value, self.header.result())?;
        let returned = self.ir.add_expr(IrExpr::Return(Some(value)));
        // The function's own return leaves the innermost callable.
        self.ir.checked_return_depths.insert(returned, 0);
        Ok(returned)
    }

    /// An `if`-`else`: a condition branch and the `else` branch, which a KLIB serializes as a
    /// final branch whose condition is the constant `true`. It lowers as checked FIR lowers a
    /// source conditional. The branches are lowered before the shape is checked, so a body that
    /// uses an unmodelled form inside any other `when` declines by that form.
    fn conditional(&mut self, branches: &[KlibIrBranch], ty: Ty) -> Lowered<ExprId> {
        let mut lowered = Vec::with_capacity(branches.len());
        for (index, branch) in branches.iter().enumerate() {
            let otherwise = index + 1 == branches.len()
                && matches!(
                    self.arena.expr(branch.condition).kind,
                    KlibIrExprKind::Const(KlibIrConstant::Boolean(true))
                );
            let condition = if otherwise {
                None
            } else {
                Some(self.operand(branch.condition, Ty::Boolean)?)
            };
            lowered.push((condition, self.operand(branch.result, ty)?));
        }
        if !matches!(lowered.as_slice(), [(Some(_), _), (None, _)]) {
            return unsupported("a `when` other than an `if`-`else`");
        }
        let conditional = self.ir.add_expr(IrExpr::When { branches: lowered });
        // An `if` with an `else` is exhaustive; checked FIR lowering records only that a `Unit`
        // one is a statement.
        if ty == Ty::Unit {
            self.ir.whens.exhaustive.insert(conditional, Ty::Unit);
        }
        Ok(conditional)
    }
}

fn unsupported<T>(form: &'static str) -> Lowered<T> {
    Err(KlibBodyDeclineReason::UnsupportedOperation(form))
}

fn type_operator_name(operator: KlibIrTypeOperator) -> &'static str {
    match operator {
        KlibIrTypeOperator::Cast => "a cast",
        KlibIrTypeOperator::ImplicitCast => "an implicit cast",
        KlibIrTypeOperator::ImplicitNotNull => "an implicit non-null assertion",
        KlibIrTypeOperator::ImplicitCoercionToUnit => "an implicit coercion to `Unit`",
        KlibIrTypeOperator::ImplicitIntegerCoercion => "an implicit integer coercion",
        KlibIrTypeOperator::SafeCast => "a safe cast",
        KlibIrTypeOperator::InstanceOf => "an `is` check",
        KlibIrTypeOperator::NotInstanceOf => "an `!is` check",
        KlibIrTypeOperator::SamConversion => "a SAM conversion",
        KlibIrTypeOperator::ImplicitDynamicCast => "an implicit dynamic cast",
        KlibIrTypeOperator::ReinterpretCast => "a reinterpreting cast",
    }
}
