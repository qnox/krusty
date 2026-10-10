//! A decoded KLIB body as checked common IR.
//!
//! Each modelled KLIB IR form lowers to exactly the common IR checked FIR lowering produces for the
//! equivalent source, with the same per-expression facts (logical types, return depths, binding
//! stability, `when` facts), so a backend compiles a dependency body as it compiles a module one.
//! The match over KLIB expression forms is exhaustive: every form either has a lowering here or
//! declines by its name.

use super::body_unit::Linker;
use super::decline::KlibBodyDeclineReason;
use super::function_lowering::FunctionHeader;
use super::ir_builtins::{BuiltinOperator, BuiltinRelation, IrBuiltinOperators};
use super::klib_types::semantic_type;
use super::local_values::LocalValues;
use crate::ir::{
    complete_bottom_value, Callee, ExprId, FunId, IrBinOp, IrBindingStability, IrConst, IrExpr,
    IrFile,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBranch, KlibIrExpr, KlibIrExprId, KlibIrExprKind,
    KlibIrMemberAccess, KlibIrStatement, KlibIrTypeId, KlibIrTypeOperator, KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::{EqualityMode, Ty};

type Lowered<T> = Result<T, KlibBodyDeclineReason>;

/// The unit function a call of a dependency declaration invokes, with its declared parameters
/// and result.
pub(super) struct LinkedCallee {
    pub(super) function: FunId,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) context_count: usize,
}

/// Lowers the body of one function into `ir`.
pub(super) struct BodyLowering<'a, 'l, 's> {
    arena: &'a KlibIrArena,
    /// The function whose body this is: the only target its `return`s may name.
    function: &'a KlibIrSymbol,
    header: &'a FunctionHeader,
    builtins: &'a IrBuiltinOperators,
    ir: &'a mut IrFile,
    linker: &'a mut Linker<'l, 's>,
    locals: LocalValues,
}

/// How a source `when` was written, which decides the shape checked FIR lowering gives it.
#[derive(Clone, Copy)]
enum WhenForm {
    /// `if`-`else`, chained through `else if`: checked FIR lowering nests one two-branch `when`
    /// per `if` and records only a `Unit` one as exhaustive.
    If,
    /// A subjectless `when` with an `else`: one flat `when`, recorded as exhaustive at its type.
    When,
}

impl<'a, 'l, 's> BodyLowering<'a, 'l, 's> {
    pub(super) fn new(
        arena: &'a KlibIrArena,
        function: &'a KlibIrSymbol,
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
        }
    }

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
            KlibIrStatement::Variable(variable) => self.variable(*variable),
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
            KlibIrExprKind::Block { .. } => return unsupported("a block"),
            KlibIrExprKind::Composite { .. } => return unsupported("a composite block"),
            KlibIrExprKind::ReturnableBlock { .. } => return unsupported("a returnable block"),
            KlibIrExprKind::InlinedFunctionBlock { .. } => {
                return unsupported("an inlined function block")
            }
            KlibIrExprKind::TypeOperator {
                operator: KlibIrTypeOperator::ImplicitCast,
                operand,
                argument,
            } => {
                let ty = self.ty(expression, "implicit cast")?;
                // The value as checked FIR lowering leaves it: an implicit cast between equal
                // semantic types adds no node and no fact of its own.
                return self.implicit_cast(*operand, *argument, ty);
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

    /// An implicit cast that converts nothing: its value already has the target type.
    fn implicit_cast(
        &mut self,
        operand: KlibIrTypeId,
        argument: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let target = semantic_type(self.arena, operand)?;
        if target != ty {
            return Err(mismatch("an implicit cast is typed apart from its target"));
        }
        self.operand(argument, target)
    }

    /// A call of a compiler built-in operator or of a dependency function. A built-in operator
    /// lowers as checked FIR lowers the source operator it serializes; a dependency function
    /// lowers as a call of its function in the same unit.
    fn call(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        if access.symbol.kind != KlibIrSymbolKind::Function {
            return Err(mismatch("a call target's symbol is not a function"));
        }
        let KlibIrSignature::Public(callee) = &access.symbol.signature else {
            return unsupported("a call of a file-private declaration");
        };
        let Some(operator) = self.builtins.operator(callee) else {
            if !access.type_arguments.is_empty() {
                // Only a generic declaration takes type arguments, and lowering declines those.
                return unsupported("a call of a generic declaration");
            }
            return self.dependency_call(callee, access, ty);
        };
        if !access.type_arguments.is_empty() {
            return Err(mismatch(
                "a built-in operator is called with type arguments",
            ));
        }
        match operator {
            BuiltinOperator::Relation(relation) => self.relation(access, relation, ty),
            BuiltinOperator::Not => self.negation(access, ty),
            equality => self.equality(access, equality, ty, false),
        }
    }

    /// A primitive relation over two operands of one primitive type. On `Float` and `Double` that
    /// relation is the IEEE one, as the built-in is.
    fn relation(
        &mut self,
        access: &KlibIrMemberAccess,
        relation: BuiltinRelation,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let (lhs, rhs) = binary_operands(access, ty)?;
        let lhs = self.operand(lhs, relation.operand)?;
        let rhs = self.operand(rhs, relation.operand)?;
        Ok(self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: relation.operator,
            lhs,
            rhs,
        }))
    }

    /// `==` (`EQEQ`, or `ieee754equals` on two floating-point operands) or `===` (`EQEQEQ`), or
    /// their negation when `negated`. Checked FIR lowering keeps a source `==` as an equality whose
    /// mode the checker derives from the operand types, and a source `===` as the reference
    /// comparison; the KLIB serializer chose its built-in from the same operand types, so the two
    /// must agree.
    fn equality(
        &mut self,
        access: &KlibIrMemberAccess,
        operator: BuiltinOperator,
        ty: Ty,
        negated: bool,
    ) -> Lowered<ExprId> {
        let (lhs, rhs) = binary_operands(access, ty)?;
        let lhs = self.expression(lhs)?;
        let rhs = self.expression(rhs)?;
        let mode = EqualityMode::of_source_operands(self.lowered_type(lhs), self.lowered_type(rhs));
        let op = match (negated, operator) {
            (false, BuiltinOperator::Identical) => IrBinOp::RefEq,
            (true, BuiltinOperator::Identical) => IrBinOp::RefNe,
            (false, _) => IrBinOp::Eq,
            (true, _) => IrBinOp::Ne,
        };
        match (operator, mode) {
            (BuiltinOperator::Identical, _) => {
                Ok(self.ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs }))
            }
            (BuiltinOperator::Equals, EqualityMode::Ieee754) => Err(mismatch(
                "`EQEQ` compares two operands of one floating-point type",
            )),
            (
                BuiltinOperator::Ieee754Equals { .. },
                EqualityMode::Primitive | EqualityMode::Structural,
            ) => unsupported("an IEEE 754 equality of operands other than one floating-point type"),
            (BuiltinOperator::Ieee754Equals { operand }, EqualityMode::Ieee754)
                if self.lowered_type(lhs) != operand =>
            {
                Err(mismatch(
                    "`ieee754equals` compares operands of another type than its own",
                ))
            }
            (BuiltinOperator::Equals | BuiltinOperator::Ieee754Equals { .. }, mode) => {
                Ok(self.ir.add_expr(IrExpr::Equality { op, mode, lhs, rhs }))
            }
            (BuiltinOperator::Relation(_) | BuiltinOperator::Not, _) => {
                unreachable!("only an equality operator is lowered as an equality")
            }
        }
    }

    /// `!` on a `Boolean`. A KLIB serializes a source `!=` or `!==` as the negation of the
    /// equality, marked with the source operator's origin; checked FIR lowering keeps that source
    /// operator as one negated equality, so it lowers as one.
    fn negation(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        let (&[Some(operand)], true) = (arguments(access).as_slice(), ty == Ty::Boolean) else {
            return Err(mismatch(
                "`Boolean.not` is called with another shape than its declaration's",
            ));
        };
        match access.origin.as_deref() {
            None => {
                let operand = self.operand(operand, Ty::Boolean)?;
                Ok(self.ir.add_negation(operand))
            }
            Some("EXCLEQ" | "EXCLEQEQ") => {
                let expression = self.arena.expr(operand);
                let equality = match &expression.kind {
                    KlibIrExprKind::Call {
                        access,
                        super_qualifier: None,
                    } => match &access.symbol.signature {
                        KlibIrSignature::Public(callee) => self
                            .builtins
                            .operator(callee)
                            .filter(|operator| {
                                !matches!(
                                    operator,
                                    BuiltinOperator::Relation(_) | BuiltinOperator::Not
                                )
                            })
                            .map(|operator| (access, operator)),
                        _ => None,
                    },
                    _ => None,
                };
                let Some((access, operator)) = equality else {
                    return Err(mismatch("a `!=` negates no built-in equality"));
                };
                let inner = self.ty(expression, "call")?;
                self.equality(access, operator, inner, true)
            }
            Some(_) => unsupported("a negation of another form than `!`, `!=` or `!==`"),
        }
    }

    /// A call of a dependency function, which the unit lowers too.
    fn dependency_call(
        &mut self,
        callee: &KlibPublicIdSignature,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let linked = self.linker.link(self.ir, callee)?;
        if linked.context_count != 0 && matches!(access.arguments, KlibIrArguments::Split { .. }) {
            return unsupported("a context argument in the pre-2.4 argument layout");
        }
        if ty != linked.ret {
            return Err(mismatch("a call is typed apart from its callee's result"));
        }
        let arguments = arguments(access);
        if arguments.len() != linked.params.len() {
            return Err(mismatch(
                "a call passes another number of arguments than its callee declares",
            ));
        }
        let mut args = Vec::with_capacity(arguments.len());
        for (argument, parameter) in arguments.into_iter().zip(linked.params) {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a call that leaves a parameter to its default",
            ))?;
            args.push(self.operand(argument, parameter)?);
        }
        Ok(self.ir.add_expr(IrExpr::Call {
            callee: Callee::Local(linked.function),
            dispatch_receiver: None,
            args,
        }))
    }

    /// The type recorded for an expression this lowering produced.
    fn lowered_type(&self, expression: ExprId) -> Ty {
        *self
            .ir
            .logical_types
            .get(&expression)
            .expect("every lowered expression is typed")
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

    /// A `when` with an `else`, which a KLIB serializes as a final branch whose condition is the
    /// constant `true`. It lowers as checked FIR lowers the source `if`-`else` chain or `when` its
    /// origin names. The branches are lowered before the shape is checked, so a body that uses an
    /// unmodelled form inside any other `when` declines by that form.
    fn conditional(
        &mut self,
        branches: &[KlibIrBranch],
        origin: Option<&str>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let form = match origin {
            Some("IF") => WhenForm::If,
            Some("WHEN") => WhenForm::When,
            Some("ANDAND") => return unsupported("`&&`"),
            Some("OROR") => return unsupported("`||`"),
            _ => return unsupported("a `when` of another origin than `if` or `when`"),
        };
        let mut lowered = Vec::with_capacity(branches.len());
        for (index, branch) in branches.iter().enumerate() {
            let otherwise = index + 1 == branches.len()
                && matches!(
                    self.arena.expr(branch.condition).kind,
                    KlibIrExprKind::Const(KlibIrConstant::Boolean(true))
                );
            let condition = if otherwise {
                if self.ty(self.arena.expr(branch.condition), "`else` condition")? != Ty::Boolean {
                    return Err(KlibBodyDeclineReason::ImplicitConversion);
                }
                None
            } else {
                Some(self.operand(branch.condition, Ty::Boolean)?)
            };
            lowered.push((condition, self.operand(branch.result, ty)?));
        }
        let Some(((None, otherwise), conditions)) = lowered.split_last() else {
            return unsupported("a `when` without an `else`");
        };
        if conditions.is_empty() || conditions.iter().any(|(condition, _)| condition.is_none()) {
            return unsupported("a `when` without an `else`");
        }
        match form {
            WhenForm::When => {
                let when = self.ir.add_expr(IrExpr::When { branches: lowered });
                self.ir.whens.exhaustive.insert(when, ty);
                Ok(when)
            }
            WhenForm::If => {
                // `if (a) x else if (b) y else z` is `if (a) x else (if (b) y else z)`: each inner
                // `if` is an expression of the chain's type, as the expression lowering of the
                // outermost one records it.
                let mut otherwise = *otherwise;
                let conditions = conditions.to_vec();
                for (index, (condition, result)) in conditions.into_iter().enumerate().rev() {
                    let conditional = self.ir.add_expr(IrExpr::When {
                        branches: vec![(condition, result), (None, otherwise)],
                    });
                    // An `if` with an `else` is exhaustive; checked FIR lowering records only
                    // that a `Unit` one is a statement.
                    if ty == Ty::Unit {
                        self.ir.whens.exhaustive.insert(conditional, Ty::Unit);
                    }
                    otherwise = if index == 0 {
                        conditional
                    } else {
                        self.ir.logical_types.insert(conditional, ty);
                        let completed = complete_bottom_value(self.ir, conditional, ty);
                        self.ir.logical_types.insert(completed, ty);
                        completed
                    };
                }
                Ok(otherwise)
            }
        }
    }
}

/// A call's arguments in the order of its callee's parameters: dispatch receiver, context
/// parameters, extension receiver, regular parameters. A KLIB older than Kotlin 2.4 keeps the
/// receivers apart and the context arguments among the values; its context arguments are not
/// placed here, and a caller of a declaration with context parameters declines that layout.
fn arguments(access: &KlibIrMemberAccess) -> Vec<Option<KlibIrExprId>> {
    match &access.arguments {
        KlibIrArguments::Flat(arguments) => arguments.clone(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => dispatch_receiver
            .iter()
            .chain(extension_receiver)
            .map(|receiver| Some(*receiver))
            .chain(values.iter().copied())
            .collect(),
    }
}

/// The two operands of a built-in binary operator answering `ty`, which must be `Boolean`.
fn binary_operands(access: &KlibIrMemberAccess, ty: Ty) -> Lowered<(KlibIrExprId, KlibIrExprId)> {
    match (arguments(access).as_slice(), ty == Ty::Boolean) {
        (&[Some(lhs), Some(rhs)], true) => Ok((lhs, rhs)),
        _ => Err(mismatch(
            "a built-in operator is called with another shape than its declaration's",
        )),
    }
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
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
