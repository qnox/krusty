//! A call to a classpath inline function, inlined by the port of kotlinc's `MethodInliner`
//! ([`crate::jvm::inliner`]): the call-site half of kotlinc's `IrInlineCodegen`.
//!
//! Each argument is evaluated and stored to its temporary as soon as it is evaluated
//! (`genValueAndPut`), unless it is read from the caller local it already lives in, or — for an
//! `@InlineOnly` callee whose body loads its parameters first — evaluated in place where the body
//! loads it (`InplaceArgumentsMethodTransformer`). The inlined body then follows.
//!
//! This path owns bodies without inline-lambda arguments. A public inline call it cannot transform
//! remains a legal direct call; a splice-only declaration fails explicitly. The legacy byte splicer
//! is not a fallback for this route.

use std::collections::HashMap;

use super::*;
use crate::jvm::inliner::{self, Binding, Parameter, Parameters};
use crate::jvm::method_node::{
    encode_instruction, is_terminal, stack_shapes, word_delta, Category, Insn, MethodNode, Node,
};

const ACC_STATIC: u16 = 0x0008;

/// How the call site supplies one parameter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Supply {
    /// Evaluated and stored to its temporary before the body.
    Stored,
    /// Evaluated where the body first loads its temporary.
    InPlace,
    /// Read from the caller local the argument lives in.
    CallerLocal,
}

impl Emitter<'_> {
    /// Temporary migration bridge for a literal lambda the inline body uses as a value (for
    /// example, the implementation object created by `Continuation(context, block)`). The
    /// MethodNode route will own this once anonymous-object regeneration lands. This operation is
    /// deliberately unavailable to no-lambda calls, so it cannot become their fallback again.
    pub(super) fn try_inline_materialized_lambda_body(
        &mut self,
        call_expression: u32,
        target: &InlineStaticTarget<'_>,
        args: &[u32],
        leading_non_argument_operands: usize,
        body: &crate::jvm::classreader::MethodCode,
        reified: &crate::jvm::inline::ReifiedArguments,
        code: &mut CodeBuilder,
    ) -> Option<()> {
        let physical = parse_descriptor_params(target.splice_desc)?;
        if physical.len() != args.len() {
            return None;
        }
        let base = self.inline_splice_base();
        let frame = crate::jvm::inline::spliced_frame(body, target.splice_desc, &[], base)?;
        let probe = crate::jvm::inline::splice_unified(
            body,
            target.splice_desc,
            base,
            &[],
            0,
            self.cw,
            reified,
        )?;
        if (!probe.handlers.is_empty() || !probe.external_branches.is_empty())
            && code.stack_height() != 0
        {
            return None;
        }
        self.emit_call_descriptor_operands(
            call_expression,
            leading_non_argument_operands,
            args,
            &physical,
            code,
        )
        .ok()?;
        let argument_words = physical.iter().map(|&ty| i32::from(slot_words(ty))).sum();
        let result_words = descriptor_ret_words(target.descriptor);
        let splice_start = code.bytes.len();
        let rewritten = if probe.needs_relayout {
            crate::jvm::inline::splice_unified(
                body,
                target.splice_desc,
                base,
                &[],
                splice_start,
                self.cw,
                reified,
            )?
        } else {
            probe
        };
        bind_inline_handlers(code, &rewritten.handlers);
        let result_words = if rewritten.falls_through {
            result_words
        } else {
            0
        };
        code.splice_inline(
            &rewritten.bytes,
            &rewritten.external_branches,
            body.max_stack + rewritten.stack_growth,
            frame.top_local,
            argument_words,
            result_words,
            rewritten.falls_through,
        );
        self.frame.reserve_through(frame.top_local);
        self.record_spliced_lines(
            &rewritten.lines,
            body,
            target.inline_only,
            if rewritten.needs_relayout {
                0
            } else {
                splice_start
            },
            code,
        );
        self.record_spliced_locals(
            &rewritten.locals,
            target.inline_only,
            if rewritten.needs_relayout {
                0
            } else {
                splice_start
            },
            code,
        );
        Some(())
    }

    /// Where an inlined body's frame starts: kotlinc's frame size at the call, above every live
    /// local and temporary. Not the method's `max_locals`: a block that ended before the call has
    /// handed its slots back, and the inlined body reuses them as kotlinc's does.
    fn inline_splice_base(&self) -> u16 {
        self.temporaries
            .live()
            .into_iter()
            .map(|(slot, ty)| slot + slot_words(ty))
            .fold(self.frame.size(), u16::max)
    }

    /// Inline `target` through the ported inliner. `None` when this path does not cover a live call;
    /// `Some(())` once the inlined code is written, or when the call is already unreachable and
    /// therefore needs no bytecode.
    pub(super) fn try_inline_classpath_body(
        &mut self,
        call_expression: u32,
        target: &InlineStaticTarget<'_>,
        args: &[u32],
        leading_non_argument_operands: usize,
        body: &crate::jvm::classreader::MethodCode,
        reified: &crate::jvm::inline::ReifiedArguments,
        code: &mut CodeBuilder,
    ) -> Option<()> {
        if code.is_dead() {
            return Some(());
        }
        let callee = match MethodNode::read(ACC_STATIC, target.name, target.splice_desc, body) {
            Ok(callee) => callee,
            Err(error) => {
                crate::trace_compiler!("splice", "unified inliner cannot read the body: {error:?}");
                return None;
            }
        };
        if let Some(shape) = inliner::unsupported_shape(&callee) {
            crate::trace_compiler!("splice", "unified inliner declines {shape:?}");
            return None;
        }
        if inliner::requires_empty_stack_on_entry(&callee) && code.stack_height() != 0 {
            return None;
        }
        let physical = parse_descriptor_params(target.splice_desc)?;
        if physical.len() != args.len() {
            return None;
        }
        let supplies = self.parameter_supplies(
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            &physical,
            &callee,
        );
        let Some(supplies) = supplies else {
            crate::trace_compiler!("splice", "unified inliner cannot bind the arguments");
            return None;
        };
        let parameters = Parameters {
            parameters: supplies
                .iter()
                .zip(&physical)
                .zip(args)
                .map(|((supply, &ty), &argument)| Parameter {
                    category: Category::of_descriptor(&type_descriptor(ty)),
                    binding: match supply {
                        Supply::CallerLocal => self.caller_local_binding(argument, ty),
                        Supply::Stored | Supply::InPlace => Binding::Temporary,
                    },
                })
                .collect(),
        };
        let base = self.inline_splice_base();
        let inlined = match inliner::inline(&callee, &parameters, target.inline_only, base, reified)
        {
            Ok(inlined) => inlined,
            Err(error) => {
                crate::trace_compiler!("splice", "unified inliner declines: {error:?}");
                return None;
            }
        };
        let temporaries = parameters.temporaries();

        // Arguments, in order: each is stored right after it is evaluated.
        let argument_frame = self.frame.mark();
        self.frame.reserve_through(base);
        let origins = self.default_operand_origins(call_expression, args, false);
        let mut inside_run = false;
        let mut leases = Vec::new();
        let mut in_place = HashMap::new();
        for (index, _) in args.iter().enumerate() {
            let Some(temporary) = temporaries[index] else {
                continue;
            };
            let slot = base + temporary;
            if supplies[index] == Supply::InPlace {
                in_place.insert(slot, index);
                continue;
            }
            self.frame.reserve_through(slot);
            let operand_frame = self.frame.mark();
            self.mark_synthesized_operand_run(
                Some((call_expression, &origins)),
                index,
                &mut inside_run,
                code,
            );
            self.emit_call_operand(
                call_expression,
                leading_non_argument_operands,
                args,
                &physical,
                index,
                code,
            );
            self.frame.rewind_to(operand_frame);
            let entered = self
                .frame
                .enter_temp(TempRole::InlineArgument, physical[index]);
            debug_assert_eq!(entered.slot(), slot);
            store(physical[index], slot, code);
            leases.push(self.lease_frame_temporary(entered, physical[index]));
        }

        // Like any call, an inline call states its line once its arguments are evaluated; that line
        // is the call site the body's lines are mapped against.
        debug_lines::mark_expression_start(self.ir, call_expression, code);
        let caller_line = code.current_line();
        let call_line = caller_line.unwrap_or(1);
        let placement = InlinePlacement {
            call_expression,
            args,
            leading_non_argument_operands,
            physical: &physical,
            in_place,
            origins: &origins,
        };
        self.write_inlined_node(
            &inlined,
            body,
            target.inline_only,
            call_line,
            placement,
            code,
        );
        // kotlinc's `markLineNumberAfterInlineIfNeeded`: inside a condition the caller's line is
        // marked again for the jump that follows; elsewhere it is forgotten, so the next mark of
        // any line is written.
        match caller_line {
            Some(line) if self.inside_condition => code.inlined_line(line),
            _ => code.forget_line(),
        }

        for lease in leases {
            self.release_temporary(lease);
        }
        self.frame.rewind_to(argument_frame);
        Some(())
    }

    /// kotlinc's choice per argument. An `@InlineOnly` callee reads a dispatch receiver or ordinary
    /// argument that is already a local from that local (`genOrGetLocal`); its extension receiver
    /// is always stored. When the body loads its parameters first, the stored arguments are instead
    /// evaluated in place.
    ///
    /// `None` only when a function-typed local has no provider-published materialization role. An
    /// inline parameter reads that local where it already lives; a `noinline` parameter is stored
    /// like an ordinary value.
    fn parameter_supplies(
        &self,
        call_expression: u32,
        target: &InlineStaticTarget<'_>,
        args: &[u32],
        leading_non_argument_operands: usize,
        physical: &[Ty],
        callee: &MethodNode,
    ) -> Option<Vec<Supply>> {
        let function_typed = |argument: u32| {
            let semantic = self
                .ir
                .logical_types
                .get(&argument)
                .copied()
                .unwrap_or_else(|| self.value_ty(argument));
            matches!(semantic.non_null(), Ty::Fun(_))
        };
        let in_place = target.inline_only
            && !args.is_empty()
            && !args.iter().any(|&argument| function_typed(argument))
            && inliner::can_inline_arguments_in_place(callee)
            && in_place_arguments::arguments_movable(self.ir, args);
        let mut supplies = Vec::with_capacity(args.len());
        for (index, &argument) in args.iter().enumerate() {
            let evaluated = if in_place {
                Supply::InPlace
            } else {
                Supply::Stored
            };
            let local = matches!(self.ir.expr(argument), IrExpr::GetValue(v)
                if self.slots.contains_key(v))
                && self.local_needs_no_boxing(argument, physical[index]);
            if !local {
                supplies.push(evaluated);
                continue;
            }
            if !target.inline_only {
                supplies.push(evaluated);
                continue;
            }
            let extension_receiver = self
                .ir
                .static_extension_receivers
                .get(&call_expression)
                .is_some_and(|&position| {
                    position as usize + leading_non_argument_operands == index
                });
            if extension_receiver {
                supplies.push(evaluated);
                continue;
            }
            if function_typed(argument) {
                let parameter = index.checked_sub(leading_non_argument_operands)?;
                let materialized = self
                    .ir
                    .call_materialized_lambda_params
                    .get(&call_expression)?
                    .get(parameter)
                    .copied()?;
                supplies.push(if materialized {
                    evaluated
                } else {
                    Supply::CallerLocal
                });
                continue;
            }
            supplies.push(Supply::CallerLocal);
        }
        Some(supplies)
    }

    /// kotlinc's `isLocalWithNoBoxing`: the local and the parameter are both primitive or both
    /// references, and no value class is boxed or unboxed between them.
    fn local_needs_no_boxing(&self, argument: u32, parameter: Ty) -> bool {
        let IrExpr::GetValue(value) = self.ir.expr(argument) else {
            return false;
        };
        let Some(&(_, local)) = self.slots.get(value) else {
            return false;
        };
        let primitive = |ty: Ty| !ty.is_reference() && slot_words(ty) > 0;
        if primitive(local) || primitive(parameter) {
            return local == parameter;
        }
        local.scalar_value_repr().is_none() && parameter.scalar_value_repr().is_none()
    }

    /// The caller local an argument is read from, coerced to the parameter's class as kotlinc's
    /// `StackValue.coerce` does between two reference types.
    fn caller_local_binding(&self, argument: u32, parameter: Ty) -> Binding {
        let IrExpr::GetValue(value) = self.ir.expr(argument) else {
            unreachable!("a caller-local argument is a local read");
        };
        let (slot, local) = self.slots[value];
        let category = Category::of_descriptor(&type_descriptor(local));
        let checkcast = (category == Category::Reference
            && type_descriptor(local) != type_descriptor(parameter))
        .then(|| checkcast_internal(parameter))
        .flatten();
        Binding::CallerLocal {
            slot,
            category,
            checkcast,
        }
    }

    /// Evaluate one call operand and adapt it to its descriptor type, as
    /// [`Self::emit_call_descriptor_operands`] does for every operand in turn.
    fn emit_call_operand(
        &mut self,
        call_expression: u32,
        leading_non_argument_operands: usize,
        args: &[u32],
        physical: &[Ty],
        index: usize,
        code: &mut CodeBuilder,
    ) {
        let expression = args[index];
        self.emit_value(expression, code);
        let source = self.value_ty(expression);
        match index.checked_sub(leading_non_argument_operands) {
            Some(argument_index) => self.adapt_physical_call_operand_for(
                call_expression,
                argument_index,
                expression,
                source,
                physical[index],
                code,
            ),
            None => self.adapt_physical_operand_for(expression, source, physical[index], code),
        }
    }

    /// Write an inlined node into the caller: labels become caller labels, jumps and switches are
    /// linked through them, every other instruction is encoded against the caller's pool, line
    /// numbers are mapped into the caller's source map, and an in-place argument is evaluated where
    /// the body loads its temporary.
    fn write_inlined_node(
        &mut self,
        inlined: &MethodNode,
        body: &crate::jvm::classreader::MethodCode,
        inline_only: bool,
        call_line: u16,
        mut placement: InlinePlacement<'_>,
        code: &mut CodeBuilder,
    ) {
        let shapes = stack_shapes(inlined).expect("an inlined body's stack was already followed");
        let baseline = code.stack_height();
        let labels: Vec<Label> = (0..inlined.label_count())
            .map(|_| code.new_label())
            .collect();
        let mut bound_at = vec![None; labels.len()];
        for block in &inlined.try_catch_blocks {
            let catch_type = block
                .catch_type
                .as_deref()
                .map_or(0, |class| self.cw.class_ref(class));
            code.add_exception(
                labels[block.start.index()],
                labels[block.end.index()],
                labels[block.handler.index()],
                catch_type,
            );
        }
        let claimable = u16::try_from(self.ir.source_line_count)
            .unwrap_or(u16::MAX)
            .max(1);
        let mut top_local = 0u16;
        let mut duplicating: Option<(u8, u16)> = None;
        for (at, node) in inlined.nodes.iter().enumerate() {
            match node {
                Node::Label(label) => {
                    let caller = labels[label.index()];
                    match &shapes[at] {
                        Some(stack) if code.is_dead() => {
                            code.bind_external_target(caller);
                            code.set_stack_height(baseline + words(stack));
                        }
                        Some(stack) => {
                            code.bind(caller);
                            code.set_stack_height(baseline + words(stack));
                        }
                        None => code.bind(caller),
                    }
                    bound_at[label.index()] = Some(code.bytes.len());
                }
                Node::Line { line, .. } => {
                    if let Some(line) =
                        self.map_inlined_line(body, inline_only, *line, call_line, claimable)
                    {
                        code.inlined_line(line);
                    }
                }
                Node::Insn(insn) => {
                    if let Insn::Var { op, slot } = insn {
                        top_local = top_local.max(slot + var_words(*op));
                        if (0x15..=0x19).contains(op) {
                            if duplicating == Some((*op, *slot)) {
                                code.instruction(
                                    &[if var_words(*op) == 2 { 0x5c } else { 0x59 }],
                                    var_words(*op) as i32,
                                    false,
                                );
                                continue;
                            }
                            if let Some(index) = placement.in_place.remove(slot) {
                                self.emit_in_place_argument(&placement, index, *slot, code);
                                duplicating = Some((*op, *slot));
                                continue;
                            }
                        }
                    }
                    duplicating = None;
                    if let Insn::Iinc { slot, .. } = insn {
                        top_local = top_local.max(slot + 1);
                    }
                    self.write_inlined_instruction(insn, &labels, code);
                }
            }
        }
        code.ensure_locals(top_local);
        if !self.record_locals {
            return;
        }
        for local in &inlined.local_variables {
            let (Some(start), Some(end)) =
                (bound_at[local.start.index()], bound_at[local.end.index()])
            else {
                continue;
            };
            let (Ok(start), Ok(length)) = (
                u16::try_from(start),
                u16::try_from(end.saturating_sub(start)),
            ) else {
                continue;
            };
            code.add_local_entry(start, Some(length), local.slot, &local.name, &local.desc);
        }
    }

    fn write_inlined_instruction(&mut self, insn: &Insn, labels: &[Label], code: &mut CodeBuilder) {
        let delta = word_delta(insn).expect("an inlined body's stack was already followed");
        match insn {
            Insn::Jump { op, target } => code.jump(*op, labels[target.index()], delta),
            Insn::TableSwitch {
                low,
                high,
                default,
                labels: targets,
            } => {
                let targets: Vec<Label> = targets.iter().map(|t| labels[t.index()]).collect();
                code.tableswitch(*low, *high, labels[default.index()], &targets);
            }
            Insn::LookupSwitch {
                default,
                keys,
                labels: targets,
            } => {
                let pairs: Vec<(i32, Label)> = keys
                    .iter()
                    .zip(targets)
                    .map(|(&key, t)| (key, labels[t.index()]))
                    .collect();
                code.lookupswitch(labels[default.index()], &pairs);
            }
            _ => {
                let bytes = encode_instruction(insn, self.cw)
                    .ok()
                    .flatten()
                    .expect("every non-branch instruction has a fixed encoding");
                code.instruction(&bytes, delta, is_terminal(insn));
            }
        }
    }

    fn emit_in_place_argument(
        &mut self,
        placement: &InlinePlacement<'_>,
        index: usize,
        temporary: u16,
        code: &mut CodeBuilder,
    ) {
        // kotlinc evaluated the argument before the body, with the earlier arguments' temporaries
        // already entered into its frame, and moved the code here afterwards.
        let mut inside_run = false;
        let saved = self.frame.mark();
        self.frame.reserve_through(temporary);
        self.mark_synthesized_operand_run(
            Some((placement.call_expression, placement.origins)),
            index,
            &mut inside_run,
            code,
        );
        self.emit_call_operand(
            placement.call_expression,
            placement.leading_non_argument_operands,
            placement.args,
            placement.physical,
            index,
            code,
        );
        self.frame.rewind_to(saved);
    }
}

impl Emitter<'_> {
    /// Map a line of an inlined body into the caller's source map (kotlinc's `SourceMapCopier`):
    /// through the dependency's own map when it has one, then as a range of the caller's
    /// `SMAP`. `None` for a line nothing maps: an `@InlineOnly` body's, or one whose file the
    /// dependency does not name.
    pub(super) fn map_inlined_line(
        &mut self,
        body: &crate::jvm::classreader::MethodCode,
        inline_only: bool,
        line: u16,
        call_line: u16,
        claimable: u16,
    ) -> Option<u16> {
        if inline_only {
            return None;
        }
        let source_file = body.source_file.as_deref()?;
        // The path a line is recorded under is the class the code was READ from — for a multifile
        // facade's function, the part class that holds its body, not the facade the call names.
        let path = body.defining_class.as_str();
        let (name, path, source) = match body.dependency_source_map.as_ref() {
            Some(map) => map.resolve(line)?,
            None => (source_file, path, line),
        };
        self.cw
            .source_map_for_inlining(claimable)
            .and_then(|map| map.map_line(name, path, source, call_line))
    }
}

/// What writing the inlined node needs to know about the call's arguments.
struct InlinePlacement<'a> {
    call_expression: u32,
    args: &'a [u32],
    leading_non_argument_operands: usize,
    physical: &'a [Ty],
    /// An in-place argument's temporary → its operand index; removed once evaluated.
    in_place: HashMap<u16, usize>,
    origins: &'a [crate::jvm::default_call_operands::DefaultOperandOrigin],
}

fn words(stack: &[Category]) -> i32 {
    stack.iter().map(|category| category.words()).sum()
}

fn var_words(op: u8) -> u16 {
    if matches!(op, 0x16 | 0x18 | 0x37 | 0x39) {
        2
    } else {
        1
    }
}
