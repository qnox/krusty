//! A decoded KLIB body as checked common IR.
//!
//! Each modelled KLIB IR form lowers to exactly the common IR checked FIR lowering produces for the
//! equivalent source, with the same per-expression facts (logical types, return depths, binding
//! stability, `when` facts), so a backend compiles a dependency body as it compiles a module one.
//! The match over KLIB expression forms is exhaustive: every form either has a lowering here or
//! declines by its name. The forms are lowered by responsibility: blocks and the source block
//! shape of a body ([`blocks`]), calls of built-in operators and dependency functions
//! ([`calls`]), `if`/`when` chains and `&&`/`||` ([`conditionals`]), loops and their jumps
//! ([`loops`]), string templates ([`string_templates`]) and type operators
//! ([`type_operators`]).

mod blocks;
mod calls;
mod conditionals;
mod loops;
mod string_templates;
mod type_operators;

use super::body_unit::Linker;
use super::decline::KlibBodyDeclineReason;
use super::function_lowering::FunctionHeader;
use super::ir_builtins::IrBuiltinOperators;
use super::klib_types::semantic_type;
use super::local_values::LocalValues;
use crate::ir::{
    complete_bottom_value, ExprId, FunId, IrBindingStability, IrConst, IrExpr, IrFile,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrTypeOperator,
    KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbol};
use crate::types::Ty;

type Lowered<T> = Result<T, KlibBodyDeclineReason>;

/// The unit function a call of a dependency declaration invokes, with its declared parameters
/// and result.
pub(super) struct LinkedCallee {
    pub(super) function: FunId,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) context_count: usize,
}

/// The function a body is lowered into: its serialized symbol, the only target its `return`s may
/// name, and the unit function that holds it.
#[derive(Clone, Copy)]
pub(super) struct LoweredFunction<'a> {
    pub(super) symbol: &'a KlibIrSymbol,
    pub(super) function: FunId,
}

/// Lowers the body of one function into `ir`.
pub(super) struct BodyLowering<'a, 'l, 's> {
    arena: &'a KlibIrArena,
    function: LoweredFunction<'a>,
    header: &'a FunctionHeader,
    builtins: &'a IrBuiltinOperators,
    ir: &'a mut IrFile,
    linker: &'a mut Linker<'l, 's>,
    locals: LocalValues,
    /// The loops enclosing the expression being lowered, innermost last: each serialized loop
    /// identity with the label its `break`s and `continue`s name.
    loops: Vec<(u32, String)>,
}

impl<'a, 'l, 's> BodyLowering<'a, 'l, 's> {
    pub(super) fn new(
        arena: &'a KlibIrArena,
        function: LoweredFunction<'a>,
        header: &'a FunctionHeader,
        builtins: &'a IrBuiltinOperators,
        ir: &'a mut IrFile,
        linker: &'a mut Linker<'l, 's>,
    ) -> Self {
        Self {
            arena,
            function,
            header,
            builtins,
            ir,
            linker,
            locals: LocalValues::after(header.parameter_count()),
            loops: Vec::new(),
        }
    }

    fn statement(&mut self, statement: &KlibIrStatement) -> Lowered<ExprId> {
        match statement {
            KlibIrStatement::Expression(expression) => self.discarded(*expression),
            KlibIrStatement::Variable(variable) => self.variable(*variable),
            KlibIrStatement::Function(_) => unsupported("a local function"),
            KlibIrStatement::Class(_) => unsupported("a local class"),
            KlibIrStatement::LocalDelegatedProperty(_) => unsupported("a local delegated property"),
            KlibIrStatement::TypeAlias(_) => unsupported("a local type alias"),
        }
    }

    /// `id` evaluated for its effect alone: a statement of a block, a block's trailing value or a
    /// branch of a `Unit` `when`. A KLIB coerces a value of another type to `Unit` there; the
    /// source states no conversion, and checked FIR lowering lowers the value itself.
    fn discarded(&mut self, id: KlibIrExprId) -> Lowered<ExprId> {
        let expression = self.arena.expr(id);
        if let KlibIrExprKind::TypeOperator {
            operator: KlibIrTypeOperator::ImplicitCoercionToUnit,
            operand,
            argument,
        } = &expression.kind
        {
            if semantic_type(self.arena, *operand)? != Ty::Unit
                || self.ty(expression, "coercion to `Unit`")? != Ty::Unit
            {
                return Err(mismatch("a coercion to `Unit` is typed apart from `Unit`"));
            }
            return self.expression(*argument);
        }
        self.expression(id)
    }

    fn expression(&mut self, id: KlibIrExprId) -> Lowered<ExprId> {
        let arena = self.arena;
        let expression = arena.expr(id);
        let (lowered, ty) = match &expression.kind {
            KlibIrExprKind::Const(constant) => {
                let ty = self.ty(expression, "constant")?;
                self.constant(constant, ty)?
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
            KlibIrExprKind::When { branches, origin } => {
                let ty = self.ty(expression, "`when`")?;
                (self.conditional(branches, origin.as_deref(), ty)?, ty)
            }
            KlibIrExprKind::SetValue {
                symbol,
                value,
                origin: _,
            } => {
                let ty = self.ty(expression, "assignment")?;
                (self.assignment(symbol, *value, ty)?, ty)
            }
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
            KlibIrExprKind::Block {
                statements,
                origin: None,
            } => {
                // A block's type is its source block's, which checked FIR lowering records itself.
                self.ty(expression, "block")?;
                return self.block(statements);
            }
            KlibIrExprKind::Block { .. } => {
                return unsupported("a block of a compiler-introduced form")
            }
            KlibIrExprKind::Composite { .. } => return unsupported("a composite block"),
            KlibIrExprKind::ReturnableBlock { .. } => return unsupported("a returnable block"),
            KlibIrExprKind::InlinedFunctionBlock { .. } => {
                return unsupported("an inlined function block")
            }
            KlibIrExprKind::TypeOperator {
                operator,
                operand,
                argument,
            } => {
                let ty = self.ty(expression, "type operator")?;
                return self.type_operator(*operator, *operand, *argument, ty);
            }
            KlibIrExprKind::GetField { .. } => return unsupported("a field read"),
            KlibIrExprKind::SetField { .. } => return unsupported("a field write"),
            KlibIrExprKind::GetObject(_) => return unsupported("an object value"),
            KlibIrExprKind::GetClass(_) => return unsupported("a class literal of a value"),
            KlibIrExprKind::ClassReference { .. } => return unsupported("a class literal"),
            KlibIrExprKind::GetEnumValue(_) => return unsupported("an enum entry"),
            KlibIrExprKind::Break { loop_id, label: _ } => {
                let ty = self.ty(expression, "`break`")?;
                (self.jump(*loop_id, ty, false)?, ty)
            }
            KlibIrExprKind::Continue { loop_id, label: _ } => {
                let ty = self.ty(expression, "`continue`")?;
                (self.jump(*loop_id, ty, true)?, ty)
            }
            KlibIrExprKind::While(serialized) => {
                // A loop is a statement: checked FIR lowering records no type for it.
                let ty = self.ty(expression, "loop")?;
                return self.loop_statement(serialized, false, ty);
            }
            KlibIrExprKind::DoWhile(serialized) => {
                let ty = self.ty(expression, "loop")?;
                return self.loop_statement(serialized, true, ty);
            }
            KlibIrExprKind::InstanceInitializerCall(_) => {
                return unsupported("an instance initializer call")
            }
            KlibIrExprKind::StringConcat(parts) => {
                let ty = self.ty(expression, "string template")?;
                (self.string_template(parts, ty)?, ty)
            }
            KlibIrExprKind::Throw(value) => {
                let ty = self.ty(expression, "`throw`")?;
                (self.throw(*value, ty)?, ty)
            }
            KlibIrExprKind::Try { .. } => return unsupported("`try`"),
            KlibIrExprKind::Vararg { .. } => return unsupported("a vararg argument"),
            KlibIrExprKind::FunctionExpression { .. } => return unsupported("a lambda"),
            KlibIrExprKind::DynamicMember { .. } => return unsupported("a dynamic member access"),
            KlibIrExprKind::DynamicOperator { .. } => return unsupported("a dynamic operator"),
            KlibIrExprKind::Error { .. } => return unsupported("an error expression"),
            KlibIrExprKind::ErrorCall { .. } => return unsupported("an error call"),
            KlibIrExprKind::Missing => return unsupported("an omitted argument"),
        };
        Ok(self.typed(lowered, ty))
    }

    /// Record `ty` as the checked type of the freshly lowered `lowered`, as checked FIR lowering
    /// records the type of every expression it lowers, around the bottom-value completion it
    /// attaches to a `Nothing` producer.
    fn typed(&mut self, lowered: ExprId, ty: Ty) -> ExprId {
        self.ir.logical_types.insert(lowered, ty);
        let completed = complete_bottom_value(self.ir, lowered, ty);
        self.ir.logical_types.insert(completed, ty);
        completed
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
        if self.lowered_type(lowered) != expected {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        Ok(lowered)
    }

    /// The type recorded for an expression this lowering produced.
    fn lowered_type(&self, expression: ExprId) -> Ty {
        self.ir
            .checked_type(expression)
            .expect("every lowered value is typed")
    }

    /// The constant `constant` the KLIB types `ty`, with the type checked FIR lowering records for
    /// it: `null` is serialized as `Nothing?` and typed as the null literal.
    fn constant(&mut self, constant: &KlibIrConstant, ty: Ty) -> Lowered<(ExprId, Ty)> {
        let (value, constant_ty) = match constant {
            KlibIrConstant::Null => {
                if ty != Ty::nullable(Ty::Nothing) {
                    return Err(mismatch("a `null` is not typed `Nothing?`"));
                }
                return Ok((self.ir.add_expr(IrExpr::Const(IrConst::Null)), Ty::Null));
            }
            KlibIrConstant::Boolean(value) => (IrConst::Boolean(*value), Ty::Boolean),
            KlibIrConstant::Char(value) => (IrConst::Char(*value), Ty::Char),
            KlibIrConstant::Byte(value) => (IrConst::Byte(*value), Ty::Byte),
            KlibIrConstant::Short(value) => (IrConst::Short(*value), Ty::Short),
            KlibIrConstant::Int(value) => (IrConst::Int(*value), Ty::Int),
            KlibIrConstant::Long(value) => (IrConst::Long(*value), Ty::Long),
            KlibIrConstant::Float(value) => (IrConst::Float(*value), Ty::Float),
            KlibIrConstant::Double(value) => (IrConst::Double(*value), Ty::Double),
            KlibIrConstant::String(value) => (IrConst::String(value.as_str().into()), Ty::String),
        };
        if constant_ty != ty {
            // An unsigned constant, for one, is serialized as its signed value typed `UInt`.
            return unsupported("a constant of another type than its value's");
        }
        Ok((self.ir.add_expr(IrExpr::Const(value)), ty))
    }

    fn read(&mut self, symbol: &KlibIrSymbol) -> Lowered<ExprId> {
        let (index, stability) = if let Some(local) = self.locals.get(symbol) {
            (local.index, Some(local.stability()))
        } else {
            let parameter = self
                .header
                .value(symbol)
                .ok_or(KlibBodyDeclineReason::ForeignValue)?;
            let stability = parameter.stable_read.then_some(IrBindingStability::Stable);
            (parameter.index, stability)
        };
        let read = self.ir.add_expr(IrExpr::GetValue(index));
        if let Some(stability) = stability {
            self.ir.binding_read_stability.insert(read, stability);
        }
        Ok(read)
    }

    /// A local `val` or `var` with its initializer, lowered as checked FIR lowers the source
    /// declaration: a named variable in the next value slot, with no type of its own.
    fn variable(&mut self, id: KlibIrVariableId) -> Lowered<ExprId> {
        let variable = self.arena.variable(id);
        let ty = semantic_type(self.arena, variable.ty)?;
        let local = self.locals.slot(variable, ty)?;
        let initializer =
            variable
                .initializer
                .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                    "a variable without an initializer",
                ))?;
        let init = self.operand(initializer, ty)?;
        self.locals.declare(&variable.base.symbol, local)?;
        Ok(self.ir.add_expr(IrExpr::Variable {
            index: local.index,
            ty,
            init: Some(init),
            named: true,
        }))
    }

    /// An assignment to a local `var`, typed `Unit` as the source assignment is.
    fn assignment(
        &mut self,
        symbol: &KlibIrSymbol,
        value: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let Some(local) = self.locals.get(symbol) else {
            return Err(match self.header.value(symbol) {
                Some(_) => mismatch("a body assigns a parameter"),
                None => KlibBodyDeclineReason::ForeignValue,
            });
        };
        if !local.mutable {
            return unsupported("an assignment that initializes a `val`");
        }
        if ty != Ty::Unit {
            return Err(mismatch("an assignment is not typed `Unit`"));
        }
        let value = self.operand(value, local.ty)?;
        Ok(self.ir.add_expr(IrExpr::SetValue {
            var: local.index,
            value,
        }))
    }

    fn return_value(&mut self, target: &KlibIrSymbol, value: KlibIrExprId) -> Lowered<ExprId> {
        if target != self.function.symbol {
            return Err(KlibBodyDeclineReason::ForeignReturnTarget);
        }
        let value = self.operand(value, self.header.result())?;
        let returned = self.ir.add_expr(IrExpr::Return(Some(value)));
        // The function's own return leaves the innermost callable.
        self.ir.checked_return_depths.insert(returned, 0);
        Ok(returned)
    }

    /// `throw` of the value `value`, which checked FIR lowering lowers as the source operand.
    fn throw(&mut self, value: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::Nothing {
            return Err(mismatch("a `throw` is not typed `Nothing`"));
        }
        let operand = self.expression(value)?;
        Ok(self.ir.add_expr(IrExpr::Throw { operand }))
    }
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

fn unsupported<T>(form: &'static str) -> Lowered<T> {
    Err(KlibBodyDeclineReason::UnsupportedOperation(form))
}
