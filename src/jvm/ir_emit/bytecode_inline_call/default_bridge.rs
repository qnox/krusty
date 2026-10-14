//! A call that omits arguments of a classpath inline function names the function's `$default`
//! bridge, passing a placeholder for each omitted parameter and then constant mask words and a null
//! method handle. kotlinc's `IrInlineCodegen` inlines that bridge without passing any of them: the
//! masks resolve the bridge's header ([`inliner::expand_default_masks`]), the method handle is not a
//! parameter, and an omitted parameter gets its temporary without a value (the bridge's default
//! code stores it, `ValueKind.DEFAULT_PARAMETER`). `@InlineOnly` is the declaration's, which the
//! bridge itself does not record.

use super::*;
use crate::jvm::inliner::DefaultMasks;

impl Emitter<'_> {
    /// The masks a realized default call passes, and where its bridge declares them; `None` for a
    /// call that is not a default bridge call. A recorded plan that no longer matches the operands,
    /// or a mask that is not an integer constant, is a broken realization.
    pub(super) fn default_bridge_masks(
        &self,
        call_expression: u32,
        args: &[u32],
        physical: &[Ty],
    ) -> Result<Option<DefaultMasks>, &'static str> {
        let Some(plan) = self.default_call_operands.matching(call_expression, args) else {
            if self.default_call_operands.contains(call_expression) {
                return Err("a default call's operand plan does not match its inlined operands");
            }
            return Ok(None);
        };
        let Some(suffix) = plan.iter().position(|operand| operand.is_abi_suffix()) else {
            return Ok(None);
        };
        let (marker, masks) = args[suffix..]
            .split_last()
            .ok_or("a default call passes no method handle")?;
        if !matches!(self.ir.expr(*marker), IrExpr::Const(IrConst::Null)) {
            return Err("a default call's method handle is not null");
        }
        let masks = masks
            .iter()
            .map(|&mask| match self.ir.expr(mask) {
                IrExpr::Const(IrConst::Int(bits)) => Ok(*bits),
                _ => Err("a default call's mask is not an integer constant"),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if masks.is_empty() {
            return Err("a default call passes no mask");
        }
        let mut start = 0u16;
        let mut parameters = Vec::new();
        let mut omitted = Vec::new();
        for (operand, &ty) in plan[..suffix].iter().zip(&physical[..suffix]) {
            if let Some(parameter) = operand.source_parameter {
                parameters.push((parameter as usize, start));
                if operand.origin
                    == crate::jvm::default_call_operands::DefaultOperandOrigin::Synthesized
                {
                    omitted.push(parameter as usize);
                }
            }
            start = start
                .checked_add(slot_words(ty))
                .ok_or("a default call's parameter slots overflow the JVM local range")?;
        }
        let mut expected = masks
            .iter()
            .enumerate()
            .flat_map(|(word, &bits)| {
                (0..32).filter_map(move |bit| {
                    (bits & (1i32 << bit) != 0).then_some(word * 32 + bit as usize)
                })
            })
            .collect::<Vec<_>>();
        expected.sort_unstable();
        omitted.sort_unstable();
        if omitted != expected {
            return Err("a default call's omitted parameters do not match its mask bits");
        }
        Ok(Some(DefaultMasks {
            start,
            masks,
            parameters,
        }))
    }

    /// The callee's body as the inliner takes it, beside the body as the class file declares it,
    /// on which kotlinc decides whether the arguments are evaluated in place.
    pub(super) fn inline_callee_nodes(
        &self,
        call: &ClasspathInlineCall<'_, '_>,
    ) -> Result<(MethodNode, MethodNode), &'static str> {
        let declared = MethodNode::read(
            ACC_STATIC,
            call.target.name,
            call.target.splice_desc,
            call.body,
        )
        .map_err(|_| UNREADABLE_INLINE_BODY)?;
        let mut node = declared.clone();
        if let Some(masks) = call.defaults {
            inliner::expand_default_masks(&mut node, masks).map_err(|error| {
                crate::trace_compiler!("splice", "default mask header not expanded: {error:?}");
                "a default bridge's mask header is not the shape kotlinc writes"
            })?;
        }
        Ok((declared, node))
    }

    /// kotlinc's `isInlineOnly` for a call: the declaration's `@InlineOnly`, which a default bridge
    /// call reads from the declaration it fills, never from the bridge.
    pub(super) fn call_is_inline_only(
        &self,
        call_expression: u32,
        owner: &str,
        name: &str,
        descriptor: &str,
        inline: crate::libraries::InlineKind,
    ) -> bool {
        match self.default_call_operands.declaration(call_expression) {
            Some(declaration) => declaration_is_inline_only(
                self.bodies,
                &declaration.owner.render(),
                &declaration.name,
                &declaration.descriptor,
                inline,
            ),
            None => declaration_is_inline_only(self.bodies, owner, name, descriptor, inline),
        }
    }

    /// How a default bridge call supplies the parameter at `index`: a mask word or the method
    /// handle is no parameter of the inlined body, and an omitted parameter only reserves its
    /// temporary. `None` for an argument the call passes.
    pub(super) fn default_bridge_supply(
        &self,
        call_expression: u32,
        args: &[u32],
        index: usize,
    ) -> Option<Supply> {
        let plan = self.default_call_operands.matching(call_expression, args)?;
        let operand = plan.get(index)?;
        if operand.is_abi_suffix() {
            Some(Supply::MaskOperand)
        } else if operand.source_parameter.is_some()
            && operand.origin
                == crate::jvm::default_call_operands::DefaultOperandOrigin::Synthesized
        {
            Some(Supply::Omitted)
        } else {
            None
        }
    }
}
