//! A literal lambda passed to an inline function, compiled into the method node kotlinc's inliner
//! receives for it (`IrExpressionLambdaImpl`): the lambda's parameters from slot 0, the values it
//! captures after them, its `$i$a$` marker as its first local, and its body ending in a return of
//! its own type. The inliner places that node at each `invoke` of the lambda.

use super::lambda_route::{leaves_by_non_local_jump, SpliceReason};
use super::*;
use crate::jvm::classreader::{ExcEntry, MethodLocal};
use crate::jvm::method_node::CodeAttribute;

const GETSTATIC: u8 = 0xb2;
const ARETURN: u8 = 0xb0;
const RETURN: u8 = 0xb1;

/// A lambda argument ready for the inliner, and the caller values it captures, in capture order.
pub(super) struct LambdaArgument {
    pub lambda: inliner::Lambda,
    pub captures: Vec<(u32, Ty)>,
}

/// Where a lambda's node keeps its values: the parameters from slot 0, then the captured values,
/// then the `$i$a$` marker at `args_size`. `slots` is indexed like `jvm_function_params` (captures
/// first), which is how the body's value indices name them.
struct LambdaFrame {
    slots: Vec<(u16, Ty)>,
    args_size: u16,
}

fn lambda_frame(capture_types: &[Ty], parameter_types: &[Ty]) -> LambdaFrame {
    let mut slot = 0u16;
    let mut slots = vec![(0u16, Ty::Error); capture_types.len() + parameter_types.len()];
    for (index, &ty) in parameter_types.iter().enumerate() {
        slots[capture_types.len() + index] = (slot, ty);
        slot += slot_words(ty);
    }
    for (index, &ty) in capture_types.iter().enumerate() {
        slots[index] = (slot, ty);
        slot += slot_words(ty);
    }
    LambdaFrame {
        slots,
        args_size: slot,
    }
}

/// The parts of a literal lambda argument both route planning and compilation read.
struct LiteralLambda {
    impl_fn: u32,
    captures: Vec<u32>,
    inline_body: u32,
    capture_types: Vec<Ty>,
    parameter_types: Vec<Ty>,
}

impl Emitter<'_> {
    fn literal_lambda(&self, argument: u32) -> (LiteralLambda, usize) {
        let IrExpr::Lambda {
            impl_fn,
            arity,
            captures,
            inline_body: Some(inline_body),
            ..
        } = self.ir.expr(argument).clone()
        else {
            unreachable!("only a literal lambda with an inline body is planned or compiled here");
        };
        let physical = jvm_function_params(self.ir, impl_fn);
        let split = captures.len().min(physical.len());
        let (capture_types, parameter_types) = physical.split_at(split);
        (
            LiteralLambda {
                impl_fn,
                captures,
                inline_body,
                capture_types: capture_types.to_vec(),
                parameter_types: parameter_types.to_vec(),
            },
            usize::from(arity),
        )
    }

    /// Why the call passing the literal lambda `argument` takes the splice route, if it must: the
    /// lambda shapes the MethodInliner port does not own yet. Decided from the checked IR before
    /// anything of the call is emitted.
    pub(super) fn lambda_splice_reason(&mut self, argument: u32) -> Option<SpliceReason> {
        let (lambda, arity) = self.literal_lambda(argument);
        // A suspension in the lambda needs the splice's state-machine markers until the coroutine
        // transformer runs on inlined bytecode; a suspend lambda's `Continuation` parameter is the
        // one its arity does not count.
        if self.suspends_within(lambda.inline_body) || lambda.parameter_types.len() != arity {
            return Some(SpliceReason::SuspendingLambda);
        }
        if leaves_by_non_local_jump(self.ir, lambda.inline_body) {
            return Some(SpliceReason::NonLocalJump);
        }
        let semantic: Vec<Ty> = match self.ir.logical_types.get(&argument).map(|ty| ty.non_null()) {
            Some(Ty::Fun(signature)) if signature.params.len() == arity => signature.params.clone(),
            _ => lambda.parameter_types.clone(),
        };
        if lambda
            .parameter_types
            .iter()
            .zip(&semantic)
            .any(|(&carrier, semantic)| {
                self.is_value_class_ty(semantic)
                    || semantic_scalar_adapter(*semantic, carrier) != carrier
            })
        {
            return Some(SpliceReason::ValueClassAdapter);
        }
        let declared_result = self.ir.functions[lambda.impl_fn as usize].ret;
        let result_semantic = self.lambda_result_semantic(&lambda);
        let declared = &self.ir.functions[lambda.impl_fn as usize];
        if self.is_value_class_ty(&result_semantic)
            || self.is_value_class_ty(&declared.ret)
            || declared
                .params
                .iter()
                .skip(lambda.captures.len())
                .any(|ty| self.is_value_class_ty(ty))
        {
            return Some(SpliceReason::ValueClassAdapter);
        }
        if declared_result.is_jvm_scalar()
            && semantic_scalar_adapter(result_semantic, declared_result) != declared_result
        {
            return Some(SpliceReason::ValueClassAdapter);
        }
        let frame = lambda_frame(&lambda.capture_types, &lambda.parameter_types);
        let result = self.inline_body_result_ty(lambda.inline_body, &frame.slots);
        if semantic_scalar_adapter(result_semantic, result) != result {
            return Some(SpliceReason::ValueClassAdapter);
        }
        None
    }

    /// Whether every value the lambda `argument` captures is a caller local the inlined body can
    /// read as it is: kotlinc binds a capture to the caller's slot only without a cast.
    pub(super) fn lambda_captures_caller_locals(&self, argument: u32) -> bool {
        let (lambda, _) = self.literal_lambda(argument);
        lambda
            .captures
            .iter()
            .zip(&lambda.capture_types)
            .all(|(&capture, &ty)| {
                matches!(self.ir.expr(capture), IrExpr::GetValue(value) if self.slots.contains_key(value))
                    && !matches!(
                        self.caller_local_binding(capture, ty),
                        Binding::CallerLocal {
                            checkcast: Some(_),
                            ..
                        }
                    )
            })
    }

    fn lambda_result_semantic(&self, lambda: &LiteralLambda) -> Ty {
        self.ir
            .logical_types
            .get(&lambda.inline_body)
            .copied()
            .unwrap_or(self.ir.functions[lambda.impl_fn as usize].ret)
    }

    /// Compile the literal lambda `argument` of a call to the inline function `callee`, which route
    /// planning gave to the MethodInliner port ([`Self::lambda_splice_reason`] found no reason to
    /// splice it). An error is a broken invariant of that plan, never a cue to try another route.
    pub(super) fn inline_lambda_node(
        &mut self,
        argument: u32,
        callee: &str,
    ) -> Result<LambdaArgument, &'static str> {
        let (lambda, _) = self.literal_lambda(argument);
        let LiteralLambda {
            impl_fn,
            captures,
            inline_body,
            capture_types,
            parameter_types,
        } = lambda;
        let LambdaFrame {
            slots: parameter_slots,
            args_size,
        } = lambda_frame(&capture_types, &parameter_types);
        let declared_result = self.ir.functions[impl_fn as usize].ret;

        // The lambda's node is a method of its own: its body's locals are laid out in a frame of
        // their own, above its parameters, captures and marker.
        let caller_frame = std::mem::take(&mut self.frame);
        let states_before = self.machine_next_ordinal;
        self.frame.reserve_through(args_size + 1);
        let mut scratch = CodeBuilder::new(args_size);
        scratch.push_int(0, self.cw);
        store(Ty::Int, args_size, &mut scratch);
        let marker_start = u16::try_from(scratch.bytes.len())
            .map_err(|_| "an inline lambda's code is too long")?;
        let result = self.emit_fn_body_inline(inline_body, &parameter_slots, &mut scratch);
        self.frame = caller_frame;
        if self.machine_next_ordinal != states_before {
            return Err("a lambda planned for the inliner started a suspension");
        }
        // A `Unit` body may or may not leave `Unit.INSTANCE`; any other body leaves its value. A
        // body that always throws ends without a return.
        let return_type = match type_descriptor(result).as_str() {
            _ if scratch.is_dead() => match type_descriptor(declared_result).as_str() {
                "V" | "Lkotlin/Unit;" => "V".to_string(),
                descriptor => descriptor.to_string(),
            },
            "V" if scratch.stack_height() == 0 => {
                scratch.ret_void();
                "V".to_string()
            }
            "V" => {
                scratch.areturn();
                "Lkotlin/Unit;".to_string()
            }
            descriptor => {
                match Category::of_descriptor(descriptor) {
                    Category::Int => scratch.ireturn(),
                    Category::Long => scratch.lreturn(),
                    Category::Float => scratch.freturn(),
                    Category::Double => scratch.dreturn(),
                    Category::Reference => scratch.areturn(),
                }
                descriptor.to_string()
            }
        };
        // kotlinc closes the marker and the parameters at the end of the lambda's method, after its
        // return and after the body's own locals.
        if self.record_locals {
            let end = u16::try_from(scratch.bytes.len())
                .map_err(|_| "an inline lambda's code is too long")?;
            if let Some(origin) = self.ir.lambda_origins.get(&impl_fn) {
                let marker = crate::jvm::debug_local_names::spliced_lambda_marker_name(
                    callee,
                    &self.owner,
                    &origin.implementation_name,
                    origin.implementation_ordinal,
                );
                scratch.add_local_entry(
                    marker_start,
                    Some(end - marker_start),
                    args_size,
                    &marker,
                    "I",
                );
            }
            let identities = self.ir.fn_params.get(&impl_fn).map(|info| &info.identities);
            for (index, &ty) in parameter_types.iter().enumerate() {
                let name = identities
                    .and_then(|identities| identities.get(captures.len() + index))
                    .and_then(|identity| match identity.role {
                        // kotlinc names an extension lambda's receiver after the lambda.
                        crate::ir::IrParameterRole::ExtensionReceiver => {
                            crate::jvm::debug_local_names::render(
                                self.ir,
                                None,
                                Some(crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver {
                                    implementation: impl_fn,
                                }),
                            )
                        }
                        _ => identity.source_name.clone(),
                    });
                if let Some(name) = name {
                    let (slot, _) = parameter_slots[captures.len() + index];
                    scratch.add_local_entry(0, Some(end), slot, &name, &type_descriptor(ty));
                }
            }
        }
        scratch.link_local_branches();
        if !scratch.external_branches().is_empty() {
            return Err("a lambda planned for the inliner jumps out of its body");
        }

        let descriptor = format!(
            "({}{}){return_type}",
            parameter_types
                .iter()
                .map(|&ty| type_descriptor(ty))
                .collect::<String>(),
            capture_types
                .iter()
                .map(|&ty| type_descriptor(ty))
                .collect::<String>(),
        );
        let mut node = self
            .read_scratch(&scratch, &descriptor)
            .ok_or("an inline lambda's code cannot be read back as a method node")?;
        let return_type = return_unit_as_void(&mut node, return_type);
        Ok(LambdaArgument {
            lambda: inliner::Lambda {
                node,
                parameter_types: parameter_types
                    .iter()
                    .map(|&ty| type_descriptor(ty))
                    .collect(),
                return_type,
                captured: 0..0,
            },
            captures: captures
                .iter()
                .copied()
                .zip(capture_types.iter().copied())
                .collect(),
        })
    }

    /// Whether `root`'s subtree holds one of the current function's suspension points.
    fn suspends_within(&self, root: u32) -> bool {
        if self.machine_suspensions.is_empty() {
            return false;
        }
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if self.machine_suspensions.contains(&expression) {
                return true;
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }
        false
    }

    fn read_scratch(&self, scratch: &CodeBuilder, descriptor: &str) -> Option<MethodNode> {
        let code_len = scratch.bytes.len();
        let handlers: Vec<ExcEntry> = scratch
            .resolved_exceptions()
            .into_iter()
            .map(|(start_pc, end_pc, handler_pc, catch_type)| ExcEntry {
                start_pc,
                end_pc,
                handler_pc,
                catch_type,
            })
            .collect();
        let locals: Vec<MethodLocal> = scratch
            .local_entries()
            .iter()
            .map(|(start, length, slot, name, desc)| {
                let end =
                    length.map_or(code_len, |length| usize::from(*start) + usize::from(length));
                Some(MethodLocal {
                    start_pc: *start,
                    length: u16::try_from(end.min(code_len).checked_sub(usize::from(*start))?)
                        .ok()?,
                    slot: *slot,
                    name: name.clone(),
                    descriptor: desc.clone(),
                })
            })
            .collect::<Option<_>>()?;
        let code = CodeAttribute {
            max_stack: scratch.max_stack,
            max_locals: scratch.max_locals,
            code: &scratch.bytes,
            handlers: &handlers,
            lines: scratch.line_marks(),
            locals: &locals,
        };
        self.cw
            .read_emitted_code(ACC_STATIC, "invoke", descriptor, &code)
    }
}

/// A `Unit` lambda: krusty's body yields `Unit.INSTANCE` for the `Object` `invoke` returns, where
/// kotlinc's lambda returns `void` and the inliner loads `Unit.INSTANCE` after it. When every
/// return is such a load, the lambda is made to return `void`.
fn return_unit_as_void(node: &mut MethodNode, return_type: String) -> String {
    if return_type != "Lkotlin/Unit;" {
        return return_type;
    }
    let loads_unit = |node: &Node| {
        matches!(node, Node::Insn(Insn::Field { op: GETSTATIC, owner, name, desc })
            if owner == "kotlin/Unit" && name == "INSTANCE" && desc == "Lkotlin/Unit;")
    };
    let returns: Vec<usize> = node
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node, Node::Insn(Insn::Op(ARETURN))))
        .map(|(at, _)| at)
        .collect();
    if returns
        .iter()
        .any(|&at| at == 0 || !loads_unit(&node.nodes[at - 1]))
    {
        return return_type;
    }
    for &at in returns.iter().rev() {
        node.nodes[at] = Node::Insn(Insn::Op(RETURN));
        node.nodes.remove(at - 1);
    }
    node.desc = format!(
        "{}V",
        &node.desc[..=node.desc.find(')').expect("a method descriptor")]
    );
    "V".to_string()
}
