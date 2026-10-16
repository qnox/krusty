//! A call that omits arguments of a classpath inline function names the function's `$default`
//! bridge, passing a placeholder for each omitted parameter and then constant mask words and a null
//! method handle. kotlinc's `IrInlineCodegen` inlines that bridge without passing any of them: the
//! masks resolve the bridge's header ([`inliner::expand_default_masks`]), the method handle is not a
//! parameter, and an omitted parameter gets its temporary without a value (the bridge's default
//! code stores it, `ValueKind.DEFAULT_PARAMETER`). `@InlineOnly` is the declaration's, which the
//! bridge itself does not record.

use super::*;
use crate::jvm::inliner::DefaultMasks;

/// A classpath inline callee's node as the class file declares it and as the inliner takes it.
pub(super) struct CalleeNodes {
    pub declared: MethodNode,
    pub callee: MethodNode,
    pub default_lambdas: Vec<inliner::ExtractedDefaultLambda>,
}

impl Emitter<'_> {
    /// The masks a realized default call passes, and where its bridge declares them; `None` for a
    /// call that is not a default bridge call. A recorded plan that no longer matches the operands,
    /// or a mask that is not an integer constant, is a broken realization. An omitted inline lambda
    /// parameter is one whose default the inlined body takes as a lambda.
    pub(super) fn default_bridge_masks(
        &self,
        call_expression: u32,
        args: &[u32],
        leading_non_argument_operands: usize,
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
        let mut inline_lambdas = Vec::new();
        for (index, (operand, &ty)) in plan[..suffix].iter().zip(&physical[..suffix]).enumerate() {
            if let Some(parameter) = operand.source_parameter {
                parameters.push((parameter as usize, start));
                if operand.origin
                    == crate::jvm::default_call_operands::DefaultOperandOrigin::Synthesized
                {
                    omitted.push(parameter as usize);
                    let inline_parameter = index
                        .checked_sub(leading_non_argument_operands)
                        .and_then(|operand| {
                            super::super::inline_parameters::is_inline_parameter(
                                self.ir,
                                call_expression,
                                operand,
                            )
                        });
                    if inline_parameter == Some(true) {
                        inline_lambdas.push(start);
                    }
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
            inline_lambdas,
        }))
    }

    /// The callee's body as the inliner takes it, beside the body as the class file declares it,
    /// on which kotlinc decides whether the arguments are evaluated in place, and the omitted
    /// parameters whose default lambdas the body takes inlined.
    pub(super) fn inline_callee_nodes(
        &self,
        call: &ClasspathInlineCall<'_, '_>,
    ) -> Result<CalleeNodes, &'static str> {
        let declared = MethodNode::read(
            ACC_STATIC,
            call.target.name,
            call.target.splice_desc,
            call.body,
        )
        .map_err(|_| UNREADABLE_INLINE_BODY)?;
        let mut callee = declared.clone();
        let default_lambdas = match call.defaults {
            Some(masks) => inliner::expand_default_masks(&mut callee, masks).map_err(|error| {
                crate::trace_compiler!("splice", "default mask header not expanded: {error:?}");
                "a default bridge's mask header is not the shape kotlinc writes"
            })?,
            None => Vec::new(),
        };
        Ok(CalleeNodes {
            declared,
            callee,
            default_lambdas,
        })
    }

    /// Each extracted default lambda read from its class, by the call operand of its parameter.
    /// The lambdas take no captured values.
    pub(super) fn load_default_lambdas(
        &self,
        call: &ClasspathInlineCall<'_, '_>,
        extracted: &[inliner::ExtractedDefaultLambda],
        physical: &[Ty],
    ) -> Result<Vec<(usize, inliner::Lambda)>, inliner::InlineError> {
        let function = call
            .target
            .name
            .strip_suffix("$default")
            .unwrap_or(call.target.name);
        extracted
            .iter()
            .map(|lambda| {
                let mut slot = 0u16;
                let operand = physical
                    .iter()
                    .position(|&ty| {
                        let found = slot == lambda.slot;
                        slot += slot_words(ty);
                        found
                    })
                    .ok_or(inliner::InlineError::DefaultLambda(
                        "a default lambda's parameter is no operand of the call",
                    ))?;
                let bytes = self.bodies.class_file(&lambda.class).ok_or(
                    inliner::InlineError::DefaultLambda(
                        "a default lambda whose class file is not on the classpath",
                    ),
                )?;
                let class = crate::jvm::class_node::ClassNode::read(&bytes).map_err(|_| {
                    inliner::InlineError::DefaultLambda("a default lambda class that does not read")
                })?;
                let loaded = inliner::load_default_lambda(&class, lambda, function, call.reified)?;
                Ok((operand, loaded))
            })
            .collect()
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
