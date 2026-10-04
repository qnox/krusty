//! A literal inline lambda that another inlined lambda captures.
//!
//! When an inline function's lambda parameter is invoked from inside a lambda the function passes
//! to another inline call (`inline fun foo(x: (Int) -> Unit) { run { x(1) } }`), expanding `foo`
//! leaves the caller's literal for `x` as a capture of `run`'s lambda. kotlinc inlines `run` into
//! `foo` first, so the literal's body replaces each `x.invoke` in the inlined code, and a `return`
//! in it still leaves the caller. The splice places the capturing lambda's body in the caller's
//! frame, so each invocation of the captured literal places the literal's own body there too: its
//! captures read the caller's slots, and its arguments cross `invoke` as they would through
//! `FunctionN`.
//!
//! Once a literal is placed, every fact the placement reads — a slot for each caller local it
//! captures, a method parameter for each capture and argument, a semantic type for each argument,
//! and its body's checked result type — is a contract of the checked IR. A missing or mis-sized
//! fact is an emission error: the backend never reconstructs it from the physical representation,
//! and never switches the call to another lowering.

use std::collections::HashMap;

use super::*;

/// A captured literal lambda, bound where the body that captures it is spliced: the literal, and
/// how each of its own captures reaches that frame.
#[derive(Clone, Debug)]
pub(super) struct InlineLambdaAlias {
    lambda: ExprId,
    bindings: CaptureBindings,
}

/// How each capture of one spliced lambda body reaches the caller's frame: the slot of each
/// ordinary value, positionally, and the literal lambda behind each capture that is one.
#[derive(Clone, Debug, Default)]
pub(super) struct CaptureBindings {
    /// The slot of each capture; an aliased capture has none, and its entry is never read.
    pub(super) slots: Vec<(u16, Ty)>,
    /// Capture position → the literal lambda it holds.
    pub(super) aliases: HashMap<u32, InlineLambdaAlias>,
}

impl CaptureBindings {
    /// One past the highest slot any capture occupies, nested aliases included: the body reads
    /// all of them, so nothing it stores may land below this.
    pub(super) fn ceiling(&self) -> u16 {
        self.slots
            .iter()
            .enumerate()
            .filter(|(position, _)| !self.aliases.contains_key(&(*position as u32)))
            .map(|(_, &(slot, ty))| slot + slot_words(ty))
            .chain(self.aliases.values().map(|alias| alias.bindings.ceiling()))
            .max()
            .unwrap_or(0)
    }
}

/// Bind the captures of the literal lambda `lambda`, whose body is spliced into the frame whose
/// value slots `slot_of` reads. A caller local is read where it lives; a literal inline lambda the
/// body only invokes is bound as an alias; any other value is evaluated into a temporary through
/// `materialize`, as the lambda's creation would have evaluated it.
pub(super) fn bind_spliced_captures(
    ir: &IrFile,
    lambda: ExprId,
    slot_of: &dyn Fn(u32) -> Option<u16>,
    materialize: &mut dyn FnMut(ExprId, Ty) -> u16,
) -> Result<CaptureBindings, &'static str> {
    let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body: Some(_),
        ..
    } = ir.expr(lambda)
    else {
        return Err("a placed lambda is not a literal inline lambda");
    };
    let physical = jvm_function_params(ir, *impl_fn);
    if physical.len() < captures.len() {
        return Err("a placed lambda's method takes fewer parameters than it has captures");
    }
    let mut bindings = CaptureBindings::default();
    for (position, &capture) in captures.iter().enumerate() {
        let ty = physical[position];
        if crate::jvm::placed_lambda_captures::is_placed_capture(ir, lambda, position) {
            let nested = bind_spliced_captures(ir, capture, slot_of, materialize)?;
            bindings.aliases.insert(
                position as u32,
                InlineLambdaAlias {
                    lambda: capture,
                    bindings: nested,
                },
            );
            bindings.slots.push((0, ty));
            continue;
        }
        let slot = match ir.expr(capture) {
            IrExpr::GetValue(value) => slot_of(*value)
                .ok_or("a placed lambda captures a caller value that has no slot in its frame")?,
            _ => materialize(capture, ty),
        };
        bindings.slots.push((slot, ty));
    }
    Ok(bindings)
}

/// The checked facts one invocation of a placed literal reads, validated against its method before
/// any code is emitted.
#[derive(Debug, PartialEq)]
pub(super) struct AliasedInvocation {
    /// The literal's checked body.
    inline_body: ExprId,
    /// How many captures precede the arguments among the method's parameters.
    captures: usize,
    /// The method parameter each argument is stored to.
    physical: Vec<Ty>,
    /// The semantic type of each argument, which selects how it crosses `invoke`.
    semantic: Vec<Ty>,
    /// The checked type of the body's value.
    result: Ty,
}

/// Validate the placement of the literal lambda `lambda` at an invocation that passes `arguments`
/// values whose semantic parameter types are `params`.
pub(super) fn aliased_invocation(
    ir: &IrFile,
    lambda: ExprId,
    params: &[Ty],
    arguments: usize,
) -> Result<AliasedInvocation, &'static str> {
    let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body: Some(inline_body),
        ..
    } = ir.expr(lambda)
    else {
        return Err("an aliased lambda is not a literal inline lambda");
    };
    let physical = jvm_function_params(ir, *impl_fn);
    let Some(parameter_types) = physical.get(captures.len()..) else {
        return Err("an aliased lambda's method takes fewer parameters than it has captures");
    };
    if parameter_types.len() != arguments {
        return Err("an aliased lambda is invoked with another arity");
    }
    if params.len() != arguments {
        return Err("an aliased lambda's invocation has no semantic type for each argument");
    }
    let Some(&result) = ir.logical_types.get(inline_body) else {
        return Err("an aliased lambda's body has no checked result type");
    };
    Ok(AliasedInvocation {
        inline_body: *inline_body,
        captures: captures.len(),
        physical: parameter_types.to_vec(),
        semantic: params.to_vec(),
        result,
    })
}

impl Emitter<'_> {
    /// Bind the captures of the literal lambda `lambda`, operand `operand` of the call placing its
    /// body in this frame. Each capture that is not a caller local is materialized into a temporary
    /// of the call's frame, recorded in `materializations` for the call to evaluate in argument
    /// order. `None` after reporting an emission error when a capture fact is missing.
    pub(super) fn bind_placed_captures(
        &mut self,
        lambda: ExprId,
        operand: usize,
        materializations: &mut Vec<(usize, ExprId, u16, Ty)>,
    ) -> Option<CaptureBindings> {
        let caller_slots = &self.slots;
        let frame = &mut self.frame;
        let bound = bind_spliced_captures(
            self.ir,
            lambda,
            &|value| caller_slots.get(&value).map(|&(slot, _)| slot),
            &mut |capture, ty| {
                let slot = frame
                    .enter_temp(frame_map::TempRole::LambdaCapture, ty)
                    .slot();
                materializations.push((operand, capture, slot, ty));
                slot
            },
        );
        bound
            .map_err(|reason| self.run.set_emit_error(reason.to_string()))
            .ok()
    }

    /// Invoke the captured literal lambda `alias` with `args`, by placing its body here. Each
    /// argument crosses `invoke` as it would through `FunctionN`: boxed to `Object`, then coerced
    /// to the parameter the body takes. The body's result is left as the `Object` `invoke` returns,
    /// for the caller to narrow as it narrows any invocation.
    pub(super) fn emit_aliased_invocation(
        &mut self,
        alias: &InlineLambdaAlias,
        args: &[ExprId],
        params: &[Ty],
        code: &mut CodeBuilder,
    ) {
        let invocation = match aliased_invocation(self.ir, alias.lambda, params, args.len()) {
            Ok(invocation) => invocation,
            Err(reason) => {
                self.run.set_emit_error(reason.to_string());
                return;
            }
        };
        for (index, &argument) in args.iter().enumerate() {
            self.emit_value(argument, code);
            let carried = self.value_ty(argument);
            box_prim_free(
                self.cw,
                code,
                semantic_scalar_adapter(invocation.semantic[index], carried),
            );
        }
        let mark = self.frame.mark();
        let mut slots = alias.bindings.slots.clone();
        slots.extend(std::iter::repeat_n((0u16, Ty::Error), args.len()));
        for index in (0..args.len()).rev() {
            let carrier = self.coerce_invoke_argument(
                invocation.semantic[index],
                invocation.physical[index],
                code,
            );
            let slot = self
                .frame
                .enter_temp(frame_map::TempRole::InlineArgument, carrier)
                .slot();
            store(carrier, slot, code);
            slots[invocation.captures + index] = (slot, carrier);
        }
        let carried = self.emit_fn_body_inline_with_aliases(
            invocation.inline_body,
            &slots,
            alias.bindings.aliases.clone(),
            code,
        );
        self.coerce_invoke_result(invocation.result, carried, code);
        self.frame.rewind_to(mark);
    }

    /// The alias the current body binds to value `value`, if any.
    pub(super) fn inline_lambda_alias(&self, value: u32) -> Option<InlineLambdaAlias> {
        self.inline_lambda_aliases.get(&value).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrFunction;

    fn lambda_method(ir: &mut IrFile, params: Vec<Ty>) -> crate::ir::FunId {
        ir.add_fun(IrFunction {
            name: "placed$lambda".to_string(),
            params,
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    /// A literal lambda capturing caller value 0, whose body yields one `Int`.
    fn placed_literal(ir: &mut IrFile, method_params: Vec<Ty>) -> (ExprId, ExprId) {
        let impl_fn = lambda_method(ir, method_params);
        let capture = ir.add_expr(IrExpr::GetValue(0));
        let body = ir.add_expr(IrExpr::GetValue(1));
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures: vec![capture],
            sam: None,
            inline_body: Some(body),
        });
        (lambda, body)
    }

    fn bind(
        ir: &IrFile,
        lambda: ExprId,
        slot: Option<u16>,
    ) -> Result<Vec<(u16, Ty)>, &'static str> {
        let mut materialized = 0;
        let bound = bind_spliced_captures(ir, lambda, &|_| slot, &mut |_, _| {
            materialized += 1;
            40
        });
        assert_eq!(
            materialized, 0,
            "a caller local is never evaluated into a temporary"
        );
        bound.map(|bindings| bindings.slots)
    }

    #[test]
    fn a_placed_lambda_binds_each_capture_to_its_callers_slot() {
        let mut ir = IrFile::default();
        let (lambda, _) = placed_literal(&mut ir, vec![Ty::Long, Ty::Int]);
        assert_eq!(bind(&ir, lambda, Some(3)), Ok(vec![(3, Ty::Long)]));
    }

    #[test]
    fn a_captured_caller_value_without_a_slot_rejects_the_placement() {
        let mut ir = IrFile::default();
        let (lambda, _) = placed_literal(&mut ir, vec![Ty::Long, Ty::Int]);
        assert_eq!(
            bind(&ir, lambda, None),
            Err("a placed lambda captures a caller value that has no slot in its frame")
        );
    }

    #[test]
    fn a_method_without_a_parameter_per_capture_rejects_the_placement() {
        let mut ir = IrFile::default();
        let (lambda, _) = placed_literal(&mut ir, Vec::new());
        assert_eq!(
            bind(&ir, lambda, Some(3)),
            Err("a placed lambda's method takes fewer parameters than it has captures")
        );
    }

    #[test]
    fn an_aliased_invocation_reads_every_fact_from_checked_ir() {
        let mut ir = IrFile::default();
        let (lambda, body) = placed_literal(&mut ir, vec![Ty::Long, Ty::Int]);
        ir.logical_types.insert(body, Ty::UInt);
        assert_eq!(
            aliased_invocation(&ir, lambda, &[Ty::UInt], 1),
            Ok(AliasedInvocation {
                inline_body: body,
                captures: 1,
                physical: vec![Ty::Int],
                semantic: vec![Ty::UInt],
                result: Ty::UInt,
            })
        );
    }

    #[test]
    fn a_missing_semantic_argument_type_rejects_the_invocation() {
        let mut ir = IrFile::default();
        let (lambda, body) = placed_literal(&mut ir, vec![Ty::Long, Ty::Int]);
        ir.logical_types.insert(body, Ty::Int);
        for params in [&[][..], &[Ty::Int, Ty::Int][..]] {
            assert_eq!(
                aliased_invocation(&ir, lambda, params, 1),
                Err("an aliased lambda's invocation has no semantic type for each argument")
            );
        }
    }

    #[test]
    fn a_missing_checked_result_type_rejects_the_invocation() {
        let mut ir = IrFile::default();
        let (lambda, _) = placed_literal(&mut ir, vec![Ty::Long, Ty::Int]);
        assert_eq!(
            aliased_invocation(&ir, lambda, &[Ty::Int], 1),
            Err("an aliased lambda's body has no checked result type")
        );
    }

    #[test]
    fn a_mis_sized_method_rejects_the_invocation() {
        let mut ir = IrFile::default();
        let (short, short_body) = placed_literal(&mut ir, Vec::new());
        ir.logical_types.insert(short_body, Ty::Int);
        assert_eq!(
            aliased_invocation(&ir, short, &[Ty::Int], 1),
            Err("an aliased lambda's method takes fewer parameters than it has captures")
        );
        let (long, long_body) = placed_literal(&mut ir, vec![Ty::Long, Ty::Int, Ty::Int]);
        ir.logical_types.insert(long_body, Ty::Int);
        assert_eq!(
            aliased_invocation(&ir, long, &[Ty::Int], 1),
            Err("an aliased lambda is invoked with another arity")
        );
    }
}
