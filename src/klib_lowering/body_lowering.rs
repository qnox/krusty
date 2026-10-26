//! A decoded KLIB body as checked common IR.
//!
//! Each modelled KLIB IR form lowers to exactly the common IR checked FIR lowering produces for the
//! equivalent source, with the same per-expression facts (logical types, return depths, binding
//! stability, `when` facts), so a backend compiles a dependency body as it compiles a module one.
//! The match over KLIB expression forms is exhaustive: every form either has a lowering here or
//! declines by its name. The forms are lowered by responsibility: blocks and the source block
//! shape of a body ([`blocks`]), the blocks of calls a prepared body inlined
//! ([`compiler_blocks`]), calls of built-in operators and dependency functions
//! ([`calls`]), constructor calls ([`constructions`]), `if`/`when` chains and `&&`/`||` ([`conditionals`]), loops and their jumps
//! ([`loops`]), string templates ([`string_templates`]) and type operators
//! ([`type_operators`]).

use crate::fx_hash::{HashMap, HashSet};

mod arrays;
mod blocks;
mod calls;
mod class_initializers;
mod class_literals;
mod closures;
mod compiler_blocks;
mod conditionals;
mod constructions;
mod loops;
mod range_membership;
mod string_templates;
mod try_catch;
mod type_operators;

use super::decline::KlibBodyDeclineReason;
use super::function_lowering::FunctionHeader;
use super::ir_builtins::IrBuiltinOperators;
use super::klib_types::{semantic_type, KlibTypeParameters};
use super::local_values::LocalValues;
use crate::ir::{
    complete_bottom_value, Callee, ExprId, FunId, IrBindingStability, IrConst, IrExpr, IrFile,
    IrTypeOp,
};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrTypeOperator,
    KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbol};
use crate::types::{Ty, TypeName};

type Lowered<T> = Result<T, KlibBodyDeclineReason>;

/// The unit function a call of a dependency declaration invokes, with its declared parameters
/// and result.
pub(super) struct LinkedCallee {
    pub(super) callee: Callee,
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) context_count: usize,
    /// Exact classifier of an external singleton dispatch selected by an earlier support unit.
    pub(super) singleton_dispatch: Option<TypeName>,
    /// The semantic identities of the callee's own type parameters, which the receiver,
    /// parameters and result name and a call's type arguments bind, in declaration order.
    pub(super) type_parameters: Vec<String>,
    /// The semantic identities of the type parameters of a member's generic class, which the
    /// receiver (the class applied to them), parameters and result name and a call binds from its
    /// dispatch receiver's type, in declaration order.
    pub(super) class_type_parameters: Vec<String>,
}

/// The declared shape of a member a range literal's membership calls.
pub(super) struct RangeMembershipHeader {
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
}

/// Joins the calls a body makes of public dependency declarations to the functions they invoke.
pub(super) trait CallLinker {
    /// The same authenticated primitive call shape that dependency planning treats as a leaf.
    /// Template linkers retain their ordinary checked external-call boundary by default.
    fn common_primitive(
        &self,
        _callee: &crate::libraries::KlibDeclarationSignature,
    ) -> Lowered<Option<super::primitive_calls::CommonPrimitiveCall>> {
        Ok(None)
    }

    /// Whether a checked source edge or retained external export needs this exact declaration.
    /// Such calls must remain callable edges; only a compiler builtin without either obligation
    /// may be folded into a common-IR operation.
    fn requires_declaration(&self, callee: &KlibPublicIdSignature) -> Lowered<bool>;

    /// The serialized header of the member `callee` an `in` over a range literal calls, read
    /// only to prove it is the range's own membership operator: its dispatch receiver, value
    /// parameters and result. Template linkers keep membership an ordinary checked call.
    fn range_membership_header(
        &self,
        _callee: &KlibPublicIdSignature,
    ) -> Lowered<Option<RangeMembershipHeader>> {
        Ok(None)
    }

    /// The function a call of the public declaration `callee` invokes.
    fn link(&mut self, ir: &mut IrFile, callee: &KlibPublicIdSignature) -> Lowered<LinkedCallee>;

    /// The dependency constructor a constructor call of the public declaration `callee` invokes.
    fn link_constructor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Lowered<LinkedConstructor>;

    /// The dependency property a call of its accessor `callee` reads or writes.
    fn link_accessor(&mut self, callee: &KlibAccessorIdSignature) -> Lowered<LinkedAccessor>;

    /// Proves that `classifier` is a dependency object, whose value a body may read.
    fn singleton(&self, classifier: TypeName) -> Lowered<()>;

    /// `sub` as the class `owner` it is a subtype of, with the type arguments its hierarchy applies
    /// to `owner`.
    fn applied_supertype(&self, sub: Ty, owner: TypeName) -> Option<Ty>;

    /// Whether `sub` is a subtype of `sup` in the calling compilation's semantic model, which a
    /// producer-checked body uses a value of the one where the other is expected for.
    fn is_subtype(&self, sub: Ty, sup: Ty) -> bool;

    /// Constructor generic bounds consume explicit hierarchy/full-formal evidence without
    /// changing the independent operand-coercion capability.
    fn constructor_bound(&self, parameters: &KlibTypeParameters, argument: Ty, bound: Ty) -> bool;
}

/// The dependency constructor a constructor call invokes: the provider's identity, the class it
/// constructs and its declared value parameters.
pub(super) struct LinkedConstructor {
    pub(super) target: crate::fir::ExternalCallableId,
    pub(super) declaration: super::constructor_calls::ConstructorDeclaration,
}

/// The dependency property a call of one of its accessors reads or writes, as a source access of
/// the property is checked: the provider's property identity, whether the accessor is its setter,
/// and the accessor's dispatch receiver, parameters (context parameters, extension receiver, and a
/// setter's value) and result.
pub(super) struct LinkedAccessor {
    pub(super) property: crate::fir::ExternalPropertyId,
    pub(super) write: bool,
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) context_count: usize,
    /// As [`LinkedCallee::class_type_parameters`].
    pub(super) class_type_parameters: Vec<String>,
}

/// The function a body is lowered into: its serialized symbol, the only target its `return`s may
/// name, and the unit function that holds it.
#[derive(Clone, Copy)]
pub(super) struct LoweredFunction<'a> {
    pub(super) symbol: &'a KlibIrSymbol,
    pub(super) function: FunId,
}

/// Lowers the body of one function into `ir`.
pub(super) struct BodyLowering<'a> {
    arena: &'a KlibIrArena,
    function: LoweredFunction<'a>,
    header: &'a FunctionHeader,
    builtins: &'a IrBuiltinOperators,
    ir: &'a mut IrFile,
    linker: &'a mut dyn CallLinker,
    locals: LocalValues,
    /// Compiler temporaries that name another immutable value, by the initializer each read of
    /// them lowers as.
    compiler_aliases: HashMap<KlibIrSymbol, KlibIrExprId>,
    /// All enclosing return-target declarations, including tail-only blocks without a frame.
    returnable_targets: Vec<KlibIrSymbol>,
    /// Enclosing serialized returnable blocks, whose exact symbols own their local exits.
    returnable_frames: Vec<compiler_blocks::ReturnableFrame>,
    /// The loops enclosing the expression being lowered, innermost last: each serialized loop
    /// identity with the label its `break`s and `continue`s name.
    loops: Vec<(u32, String)>,
    /// Loops whose lowered bodies contain a `break` that names that loop.
    breakable_loops: HashSet<u32>,
    /// The coercions of an erased body's values to their erasure, which a conversion back to the
    /// declared type cancels.
    erasure_coercions: HashSet<ExprId>,
    initialization: Option<class_initializers::InitializationFrame<'a>>,
}

impl<'a> BodyLowering<'a> {
    pub(super) fn new(
        arena: &'a KlibIrArena,
        function: LoweredFunction<'a>,
        header: &'a FunctionHeader,
        builtins: &'a IrBuiltinOperators,
        ir: &'a mut IrFile,
        linker: &'a mut dyn CallLinker,
    ) -> Self {
        Self {
            arena,
            function,
            header,
            builtins,
            ir,
            linker,
            locals: LocalValues::after(header.parameter_count()),
            compiler_aliases: HashMap::default(),
            returnable_targets: Vec::new(),
            returnable_frames: Vec::new(),
            loops: Vec::new(),
            breakable_loops: HashSet::default(),
            erasure_coercions: HashSet::default(),
            initialization: None,
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
            if semantic_type(self.arena, self.header.type_parameters(), *operand)? != Ty::Unit
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
                let read = self.read(symbol)?;
                (read, self.read_type(symbol, ty))
            }
            KlibIrExprKind::Call {
                access,
                super_qualifier,
            } => {
                let ty = self.ty(expression, "call")?;
                if super_qualifier.is_some() {
                    return unsupported("a `super` call");
                }
                let lowered =
                    match super::range_membership::range_literal_membership(arena, expression) {
                        Some(membership) => self.range_literal_membership(membership, ty, false)?,
                        None => None,
                    };
                match lowered {
                    Some(lowered) => (lowered, ty),
                    None => (self.call(access, ty)?, ty),
                }
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
            KlibIrExprKind::ConstructorCall {
                access,
                constructor_type_arguments,
            } => {
                let ty = self.ty(expression, "constructor call")?;
                (
                    self.constructor_call(access, *constructor_type_arguments, ty)?,
                    ty,
                )
            }
            KlibIrExprKind::DelegatingConstructorCall(_) => {
                return unsupported("a delegating constructor call")
            }
            KlibIrExprKind::EnumConstructorCall(_) => {
                return unsupported("an enum constructor call")
            }
            KlibIrExprKind::FunctionReference { .. } => return unsupported("a function reference"),
            KlibIrExprKind::RichFunctionReference {
                invoke,
                overridden,
                origin,
                ..
            } => {
                if !super::closure_forms::unbound_rich_reference(&expression.kind) {
                    return unsupported("a bound or reflective rich function reference");
                }
                let ty = self.ty(expression, "closure")?;
                let crate::types::TyKind::Fun(signature) = ty.kind() else {
                    return unsupported("a closure of a non-function type");
                };
                let invocation = match &overridden.signature {
                    crate::metadata::klib_ir::KlibIrSignature::Public(signature)
                        if overridden.kind
                            == crate::metadata::klib_ir::KlibIrSymbolKind::Function =>
                    {
                        super::klib_types::function_invocation_arity(signature)
                    }
                    _ => None,
                };
                if invocation != Some(signature.params.len()) {
                    return Err(mismatch(
                        "a rich closure does not override its function invocation",
                    ));
                }
                (self.closure(*invoke, ty, origin.as_deref())?, ty)
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
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if matches!(
                origin.as_str(),
                "INLINE_ARGS_CONTAINER"
                    | "POSTFIX_INCR"
                    | "POSTFIX_DECR"
                    | "PREFIX_INCR"
                    | "PREFIX_DECR"
            ) =>
            {
                let ty = self.ty(expression, "compiler expression block")?;
                return self.temporary_expression_block(statements, ty);
            }
            KlibIrExprKind::Block {
                statements,
                origin: Some(origin),
            } if origin == "FOR_LOOP" => {
                let ty = self.ty(expression, "`for` loop")?;
                return self.for_loop(statements, ty);
            }
            KlibIrExprKind::Block { .. } => {
                return unsupported("a block of a compiler-introduced form")
            }
            KlibIrExprKind::Composite { .. } => return unsupported("a composite block"),
            KlibIrExprKind::ReturnableBlock {
                symbol,
                statements,
                origin: None,
            } => {
                let ty = self.ty(expression, "returnable block")?;
                return self.returnable_block(symbol, statements, ty);
            }
            KlibIrExprKind::ReturnableBlock { .. } => {
                return unsupported("a returnable block of a compiler-introduced form")
            }
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
            KlibIrExprKind::GetField {
                symbol,
                receiver,
                super_qualifier,
                ..
            } => {
                if super_qualifier.is_some() {
                    return unsupported("a superclass field read");
                }
                let ty = self.ty(expression, "field read")?;
                return self.initialized_field(symbol, *receiver, None, ty);
            }
            KlibIrExprKind::SetField {
                symbol,
                receiver,
                value,
                super_qualifier,
                ..
            } => {
                if super_qualifier.is_some() {
                    return unsupported("a superclass field write");
                }
                let ty = self.ty(expression, "field write")?;
                return self.initialized_field(symbol, *receiver, Some(*value), ty);
            }
            KlibIrExprKind::GetObject(symbol) => {
                let ty = self.ty(expression, "object value")?;
                return self.object_value(symbol, ty);
            }
            KlibIrExprKind::GetClass(value) => {
                let ty = self.ty(expression, "class literal")?;
                return self.value_class_literal(*value, ty);
            }
            KlibIrExprKind::ClassReference { class, class_type } => {
                let ty = self.ty(expression, "class literal")?;
                return self.class_reference(class, *class_type, ty);
            }
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
            KlibIrExprKind::Try {
                result,
                catches,
                finally,
            } => {
                let ty = self.ty(expression, "`try`")?;
                (self.try_expression(*result, catches, *finally, ty)?, ty)
            }
            KlibIrExprKind::Vararg {
                element_type,
                elements,
            } => {
                let ty = self.ty(expression, "a vararg array")?;
                (self.vararg_array(*element_type, elements, ty)?, ty)
            }
            KlibIrExprKind::FunctionExpression { function, origin } => {
                let ty = self.ty(expression, "closure")?;
                (self.closure(*function, ty, origin.as_deref())?, ty)
            }
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
        semantic_type(self.arena, self.header.type_parameters(), ty)
    }

    /// Lower `id` as a value used where `expected` is. The serialized body states every
    /// conversion explicitly but the two Kotlin leaves implicit, as checked FIR does: a value is
    /// one of each of its supertypes, with no conversion, and the widening of a non-null value to
    /// a nullable type is marked with the coercion checked FIR lowering places. `null` is a value
    /// of every nullable type. Any other use, such as an erased body's value where its declared
    /// type is expected, is a conversion the body must state.
    fn operand(&mut self, id: KlibIrExprId, expected: Ty) -> Lowered<ExprId> {
        let lowered = self.expression(id)?;
        let actual = self.lowered_type(lowered);
        if is_value_of(actual, expected) {
            return Ok(lowered);
        }
        if let Some(widened) = self.nullable_widening(lowered, actual, expected) {
            return Ok(widened);
        }
        self.upcast(lowered, actual, expected)
            .ok_or(KlibBodyDeclineReason::ImplicitConversion)
    }

    /// `value`, of the type `actual`, as a value of its supertype `expected`: the value itself, or
    /// the coercion checked FIR lowering places when only `expected` is nullable. `None` when
    /// `expected` is not a supertype of `actual`.
    fn upcast(&mut self, value: ExprId, actual: Ty, expected: Ty) -> Option<ExprId> {
        if !self.linker.is_subtype(actual, expected) {
            return None;
        }
        Some(if actual.is_nullable() == expected.is_nullable() {
            value
        } else {
            self.coercion(value, expected)
        })
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
        if let Some(alias) = self.compiler_aliases.get(symbol).copied() {
            return self.expression(alias);
        }
        let (index, stability, erased) = if let Some(local) = self.locals.get(symbol) {
            (local.index, Some(local.stability()), None)
        } else {
            let parameter = self
                .header
                .value(symbol)
                .ok_or(KlibBodyDeclineReason::ForeignValue)?;
            let stability = parameter.stable_read.then_some(IrBindingStability::Stable);
            (parameter.index, stability, parameter.erased)
        };
        let read = self.ir.add_expr(IrExpr::GetValue(index));
        if let Some(stability) = stability {
            self.ir.binding_read_stability.insert(read, stability);
        }
        Ok(match erased {
            Some(erased) => {
                // The read itself is of the declared type; its use is of the erasure.
                let declared = self.header.slot_type(index);
                self.ir.logical_types.insert(read, declared);
                let coercion = self.coercion(read, erased);
                self.erasure_coercions.insert(coercion);
                coercion
            }
            None => read,
        })
    }

    /// The type a read of `symbol` the body types `serialized` has: a function-typed parameter of
    /// an erased body is read at its declared type, which its invocations convert from. The
    /// erasure also drops the markers of a function type's context parameters and receiver.
    fn read_type(&self, symbol: &KlibIrSymbol, serialized: Ty) -> Ty {
        if self.locals.get(symbol).is_some() {
            return serialized;
        }
        match self.header.value(symbol) {
            Some(parameter) if parameter.erased.is_none() => {
                let declared = self.header.slot_type(parameter.index);
                let erased = self.header.erased(declared);
                if erased == serialized
                    || super::function_lowering::plain_function_type(erased) == serialized
                {
                    declared
                } else {
                    serialized
                }
            }
            _ => serialized,
        }
    }

    /// `value`, of an erased type, as a value of the declared type `target`. A coercion of a
    /// declared value to its erasure cancels, and a conditional converts each branch, so the
    /// expanded body converts no value the source does not.
    fn declared_value(&mut self, value: ExprId, target: Ty) -> ExprId {
        let actual = self.lowered_type(value);
        if is_value_of(actual, target) || actual == Ty::Nothing {
            return value;
        }
        match self.ir.expr(value).clone() {
            IrExpr::TypeOp { arg, .. } if self.erasure_coercions.contains(&value) => {
                let inner = self.lowered_type(arg);
                if is_value_of(inner, target) {
                    return arg;
                }
                if let Some(widened) = self.nullable_widening(arg, inner, target) {
                    return widened;
                }
            }
            IrExpr::When { branches } => {
                let branches = branches
                    .into_iter()
                    .map(|(condition, result)| (condition, self.declared_value(result, target)))
                    .collect();
                self.ir.exprs[value as usize] = IrExpr::When { branches };
                self.ir.logical_types.insert(value, target);
                if self.ir.whens.exhaustive.contains_key(&value) {
                    self.ir.whens.exhaustive.insert(value, target);
                }
                return value;
            }
            _ => {}
        }
        self.coercion(value, target)
    }

    /// `value` converted to `ty` by the coercion checked FIR lowering places at a generic
    /// declaration's boundary, which the target represents.
    fn coercion(&mut self, value: ExprId, ty: Ty) -> ExprId {
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: ty,
        })
    }

    /// Lower `id` as a value of the declared type `declared` where the body produces its erasure,
    /// as a prepared-inline copy passes an erased value to a function parameter's invocation.
    fn declared_operand(&mut self, id: KlibIrExprId, declared: Ty) -> Lowered<ExprId> {
        let lowered = self.expression(id)?;
        let actual = self.lowered_type(lowered);
        if is_value_of(actual, declared) {
            return Ok(lowered);
        }
        // A prepared copy also erases the type parameters of the inline functions it already
        // expanded, so the value may be typed with the erasure's bound: a supertype of `declared`.
        if actual != self.header.erased(declared) && !self.linker.is_subtype(declared, actual) {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        Ok(self.declared_value(lowered, declared))
    }

    /// A local `val` or `var` with its initializer, lowered as checked FIR lowers the source
    /// declaration: a named variable in the next value slot, with no type of its own.
    fn variable(&mut self, id: KlibIrVariableId) -> Lowered<ExprId> {
        let variable = self.arena.variable(id);
        let ty = semantic_type(self.arena, self.header.type_parameters(), variable.ty)?;
        let initializer =
            variable
                .initializer
                .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                    "a variable without an initializer",
                ))?;
        let init = self.operand(initializer, ty)?;
        // Initializers can declare locals of their own. Allocate the enclosing declaration only
        // after those locals, as checked FIR does, and keep its symbol invisible until then.
        let local = self.locals.slot(variable, ty)?;
        self.locals.declare(&variable.base.symbol, local)?;
        self.locals
            .record_name(&variable.base.symbol, &variable.name);
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
        if let Some(returned) = self.returnable_frame_exit(target, value)? {
            return Ok(returned);
        }
        if target != self.function.symbol {
            return Err(KlibBodyDeclineReason::ForeignReturnTarget);
        }
        let mut value = self.operand(value, self.header.body_result())?;
        if self.header.body_result() != self.header.result() {
            value = self.declared_value(value, self.header.result());
        }
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

/// A value typed `actual` is one of `expected` without a conversion: the same type, or the null
/// literal where a nullable type is expected, which checked FIR lowering converts nothing for.
fn is_value_of(actual: Ty, expected: Ty) -> bool {
    actual == expected || (actual == Ty::Null && expected.is_nullable())
}
