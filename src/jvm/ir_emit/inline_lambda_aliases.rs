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

impl Emitter<'_> {
    /// Bind the captures of the literal lambda `lambda`, whose body is spliced into this frame. A
    /// caller local is read where it lives; a literal inline lambda the body only invokes is bound
    /// as an alias; any other value is evaluated into a temporary through `materialize`, as the
    /// lambda's creation would have evaluated it. `None` when a caller local has no slot.
    pub(super) fn bind_spliced_captures(
        &mut self,
        lambda: ExprId,
        materialize: &mut dyn FnMut(&mut Self, ExprId, Ty) -> u16,
    ) -> Option<CaptureBindings> {
        let IrExpr::Lambda {
            impl_fn, captures, ..
        } = self.ir.expr(lambda).clone()
        else {
            return None;
        };
        let physical = jvm_function_params(self.ir, impl_fn);
        let mut bindings = CaptureBindings::default();
        for (position, &capture) in captures.iter().enumerate() {
            let ty = *physical.get(position)?;
            if crate::jvm::placed_lambda_captures::is_placed_capture(self.ir, lambda, position) {
                let nested = self.bind_spliced_captures(capture, materialize)?;
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
            let slot = match self.ir.expr(capture) {
                IrExpr::GetValue(value) => self.slots.get(value)?.0,
                _ => materialize(self, capture, ty),
            };
            bindings.slots.push((slot, ty));
        }
        Some(bindings)
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
        let IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body: Some(inline_body),
            ..
        } = self.ir.expr(alias.lambda).clone()
        else {
            unreachable!("an alias holds a literal inline lambda");
        };
        let physical = jvm_function_params(self.ir, impl_fn);
        let parameter_types = physical[captures.len().min(physical.len())..].to_vec();
        if parameter_types.len() != args.len() {
            self.run
                .set_emit_error("an aliased lambda is invoked with another arity".to_string());
            return;
        }
        for (index, &argument) in args.iter().enumerate() {
            self.emit_value(argument, code);
            let carried = self.value_ty(argument);
            let semantic = params.get(index).copied().unwrap_or(carried);
            box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, carried));
        }
        let mark = self.frame.mark();
        let mut slots = alias.bindings.slots.clone();
        slots.extend(std::iter::repeat_n((0u16, Ty::Error), args.len()));
        for index in (0..args.len()).rev() {
            let semantic = params.get(index).copied().unwrap_or(parameter_types[index]);
            let carrier = self.coerce_invoke_argument(semantic, parameter_types[index], code);
            let slot = self
                .frame
                .enter_temp(frame_map::TempRole::InlineArgument, carrier)
                .slot();
            store(carrier, slot, code);
            slots[captures.len() + index] = (slot, carrier);
        }
        let result = self
            .ir
            .logical_types
            .get(&inline_body)
            .copied()
            .unwrap_or(self.ir.functions[impl_fn as usize].ret);
        let carried = self.emit_fn_body_inline_with_aliases(
            inline_body,
            &slots,
            alias.bindings.aliases.clone(),
            code,
        );
        self.coerce_invoke_result(result, carried, code);
        self.frame.rewind_to(mark);
    }

    /// The alias the current body binds to value `value`, if any.
    pub(super) fn inline_lambda_alias(&self, value: u32) -> Option<InlineLambdaAlias> {
        self.inline_lambda_aliases.get(&value).cloned()
    }
}
