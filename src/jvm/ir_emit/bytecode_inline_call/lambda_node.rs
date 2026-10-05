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

/// How a value crosses the `Object` an `invoke` passes or returns.
pub(super) enum InvokeCoercion {
    /// A cast, a primitive's wrapper, or nothing.
    Plain,
    /// The unboxed representation of this value class, through its `unbox-impl` and `box-impl`.
    ValueClass(String),
    /// A representation the port does not coerce yet.
    Unported,
}

impl InvokeCoercion {
    fn value_class(&self) -> Option<String> {
        match self {
            InvokeCoercion::ValueClass(class) => Some(class.clone()),
            InvokeCoercion::Plain | InvokeCoercion::Unported => None,
        }
    }
}

/// The primitive an inlined lambda stores for a non-null unsigned parameter that `invoke` passed
/// as the unsigned box. A nullable unsigned parameter stays that box.
fn unsigned_inline_carrier(semantic: Ty, physical: Ty) -> Option<Ty> {
    if semantic.is_nullable() {
        return None;
    }
    let scalar = semantic.non_null().canonical_semantic();
    if !scalar.is_unsigned() {
        return None;
    }
    let class = scalar.kotlin_class_internal()?;
    let boxed = type_descriptor(physical) == type_descriptor(Ty::nullable(scalar))
        || type_descriptor(physical) == type_descriptor(Ty::obj_name(class));
    boxed.then(|| ir_ty_to_jvm(&scalar))
}

/// `kotlin/UInt` when `carrier` is the primitive an inlined unsigned value is stored as.
fn unsigned_value_class(semantic: Ty, carrier: Ty) -> Option<String> {
    if semantic.is_nullable() {
        return None;
    }
    let scalar = semantic.non_null().canonical_semantic();
    if !scalar.is_unsigned() {
        return None;
    }
    let primitive = ir_ty_to_jvm(&scalar);
    (type_descriptor(carrier) == type_descriptor(primitive))
        .then(|| scalar.kotlin_class_internal().map(|class| class.render()))
        .flatten()
}

/// The parts of a literal lambda argument both route planning and compilation read.
struct LiteralLambda {
    impl_fn: u32,
    captures: Vec<u32>,
    inline_body: u32,
    capture_types: Vec<Ty>,
    /// What the inline body carries each parameter as: a value class unboxed, as kotlinc's inlined
    /// lambda takes it, where the implementation method takes it boxed through `invoke`.
    parameter_types: Vec<Ty>,
    /// How each parameter crosses the `Object` of `invoke` (`invokeMethodParameters`).
    parameter_coercions: Vec<InvokeCoercion>,
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
        let (capture_types, physical_parameters) = physical.split_at(split);
        let arity = usize::from(arity);
        let semantic = match self.ir.logical_types.get(&argument).map(|ty| ty.non_null()) {
            Some(Ty::Fun(signature)) if signature.params.len() == arity => signature.params.clone(),
            _ => physical_parameters.to_vec(),
        };
        let (parameter_types, parameter_coercions) = physical_parameters
            .iter()
            .zip(semantic.iter().chain(std::iter::repeat(&Ty::Error)))
            .map(|(&physical, &semantic)| self.inline_parameter(semantic, physical))
            .unzip();
        (
            LiteralLambda {
                impl_fn,
                captures,
                inline_body,
                capture_types: capture_types.to_vec(),
                parameter_types,
                parameter_coercions,
            },
            arity,
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
        if lambda
            .parameter_coercions
            .iter()
            .any(|coercion| matches!(coercion, InvokeCoercion::Unported))
        {
            return Some(SpliceReason::ValueClassAdapter);
        }
        let declared_result = self.ir.functions[lambda.impl_fn as usize].ret;
        let result_semantic = self.lambda_result_semantic(&lambda);
        if declared_result.is_jvm_scalar()
            && semantic_scalar_adapter(result_semantic, declared_result) != declared_result
        {
            return Some(SpliceReason::ValueClassAdapter);
        }
        let frame = lambda_frame(&lambda.capture_types, &lambda.parameter_types);
        let result = self.inline_body_result_ty(lambda.inline_body, &frame.slots);
        if matches!(
            self.invoke_coercion(result_semantic, result),
            InvokeCoercion::Unported
        ) {
            return Some(SpliceReason::ValueClassAdapter);
        }
        None
    }

    /// What a lambda's inline body carries its parameter of Kotlin type `semantic` as, which the
    /// implementation method takes as `physical`, and how that crosses `invoke`: a non-null value
    /// class the implementation takes boxed through `invoke`, the inline body takes unboxed.
    fn inline_parameter(&self, semantic: Ty, physical: Ty) -> (Ty, InvokeCoercion) {
        // A non-null unsigned parameter is a native scalar, so it is not a boxed value class, but
        // `FunctionN.invoke` still hands it over as `kotlin/UInt`. The inlined body takes the
        // primitive carrier and the invoke boundary unboxes through `unbox-impl`.
        if let Some(carrier) = unsigned_inline_carrier(semantic, physical) {
            return (carrier, self.invoke_coercion(semantic, carrier));
        }
        let unboxed = self
            .is_value_class_ty(&semantic)
            .then(|| semantic.non_null().obj_internal())
            .flatten()
            .filter(|&class| {
                !semantic.is_nullable()
                    && crate::jvm::names::same_type_descriptor(physical, Ty::obj_name(class))
            })
            .and_then(|class| {
                crate::jvm::value_classes::boxed_value_class_underlying(self.ir, class)
            });
        let carrier = unboxed.map_or(physical, |underlying| ir_ty_to_jvm(&underlying));
        (carrier, self.invoke_coercion(semantic, carrier))
    }

    /// `StackValue.coerce` of the `Object` an `invoke` passes to a lambda parameter of Kotlin type
    /// `semantic`, which the implementation takes as `physical`, as the byte splice places it before
    /// the inline body's parameter store: the carrier the body stores.
    pub(in crate::jvm::ir_emit) fn coerce_invoke_argument(
        &mut self,
        semantic: Ty,
        physical: Ty,
        code: &mut CodeBuilder,
    ) -> Ty {
        let (carrier, _) = self.inline_parameter(semantic, physical);
        if let Some(class) = self.unboxed_value_class(semantic, carrier) {
            let box_class = self.cw.class_ref(&class);
            code.checkcast(box_class);
            let unbox = self.cw.methodref(
                &class,
                "unbox-impl",
                &format!("(){}", type_descriptor(carrier)),
            );
            // `unbox-impl` is an instance call: a nullable value class carried unboxed (a reference
            // underlying) lets null past it, as `StackValue.coerce` does.
            null_preserving(code, semantic.is_nullable(), |code| {
                code.invokevirtual(unbox, 0, i32::from(slot_words(carrier)));
            });
        } else if carrier.is_jvm_scalar() {
            // `FunctionN.invoke` hands every argument over as `Object`. Select its adapter from the
            // lambda's semantic parameter before using the physical carrier for the local slot;
            // otherwise `UInt` is mistaken for boxed `Int` here.
            unbox_prim_from(
                self.cw,
                code,
                Ty::obj("java/lang/Object"),
                semantic_scalar_adapter(semantic, carrier),
            );
        } else if let Some(internal) = checkcast_internal(carrier) {
            let ci = self.cw.class_ref(&internal);
            code.checkcast(ci);
        }
        carrier
    }

    /// `StackValue.coerce` of the lambda body's result of Kotlin type `semantic`, which the body
    /// leaves as `carrier`, to the `Object` the replaced `invoke` returns, as the byte splice places
    /// it after the inline body.
    pub(in crate::jvm::ir_emit) fn coerce_invoke_result(
        &mut self,
        semantic: Ty,
        carrier: Ty,
        code: &mut CodeBuilder,
    ) {
        if let Some(class) = self.unboxed_value_class(semantic, carrier) {
            let boxed = self.cw.methodref(
                &class,
                "box-impl",
                &format!(
                    "({}){}",
                    type_descriptor(carrier),
                    type_descriptor(Ty::obj(&class))
                ),
            );
            null_preserving(code, semantic.is_nullable(), |code| {
                code.invokestatic(boxed, i32::from(slot_words(carrier)), 1);
            });
        } else if carrier.is_jvm_scalar() {
            // Reverse the semantic adapter the argument took: `UInt` is not a boxed `Int`.
            box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, carrier));
        }
    }

    /// The value class of Kotlin type `semantic` when `carrier` is its unboxed representation.
    fn unboxed_value_class(&self, semantic: Ty, carrier: Ty) -> Option<String> {
        let class = semantic.non_null().obj_internal()?;
        if !self.is_value_class_ty(&semantic) {
            return None;
        }
        let underlying = crate::jvm::value_classes::boxed_value_class_underlying(self.ir, class)?;
        let carried = type_descriptor(carrier);
        (carried != type_descriptor(Ty::obj_name(class))
            && carried == type_descriptor(ir_ty_to_jvm(&underlying)))
        .then(|| class.render())
    }

    /// How a lambda's value of Kotlin type `semantic`, which the lambda's node carries as
    /// `carrier`, crosses the `Object` of `invoke`: kotlinc's `StackValue.coerce` over the Kotlin
    /// types.
    fn invoke_coercion(&self, semantic: Ty, carrier: Ty) -> InvokeCoercion {
        if let Some(class) = unsigned_value_class(semantic, carrier) {
            return InvokeCoercion::ValueClass(class);
        }
        if !self.is_value_class_ty(&semantic) {
            return if semantic_scalar_adapter(semantic, carrier) == carrier {
                InvokeCoercion::Plain
            } else {
                InvokeCoercion::Unported
            };
        }
        match self.unboxed_value_class(semantic, carrier) {
            Some(class) if !semantic.is_nullable() => InvokeCoercion::ValueClass(class),
            Some(_) => InvokeCoercion::Unported,
            None if semantic.non_null().obj_internal().is_some_and(|class| {
                crate::jvm::names::same_type_descriptor(carrier, Ty::obj_name(class))
            }) =>
            {
                InvokeCoercion::Plain
            }
            None => InvokeCoercion::Unported,
        }
    }

    /// The literal lambda `argument` as route planning sees it before compiling it: its parameters,
    /// captured values and names, with a body that only returns; and the caller values it captures.
    pub(super) fn lambda_shape(&self, argument: u32) -> (inliner::Lambda, Vec<(u32, Ty)>) {
        let (lambda, _) = self.literal_lambda(argument);
        let parameter_types: Vec<String> = lambda
            .parameter_types
            .iter()
            .map(|&ty| type_descriptor(ty))
            .collect();
        let descriptor = format!(
            "({}{})V",
            parameter_types.concat(),
            lambda
                .capture_types
                .iter()
                .map(|&ty| type_descriptor(ty))
                .collect::<String>(),
        );
        let mut node = MethodNode::new(ACC_STATIC, "invoke", &descriptor);
        node.max_locals = lambda
            .parameter_types
            .iter()
            .chain(&lambda.capture_types)
            .map(|&ty| slot_words(ty))
            .sum();
        let start = node.new_label();
        node.nodes = vec![Node::Label(start), Node::Insn(Insn::Op(RETURN))];
        let shape = inliner::Lambda {
            node,
            value_class_parameters: lambda
                .parameter_coercions
                .iter()
                .map(InvokeCoercion::value_class)
                .collect(),
            parameter_types,
            return_type: "V".to_string(),
            value_class_return: None,
            captured: 0..0,
            capture_names: self.capture_names(lambda.impl_fn, lambda.captures.len()),
        };
        let captures = lambda
            .captures
            .iter()
            .copied()
            .zip(lambda.capture_types.iter().copied())
            .collect();
        (shape, captures)
    }

    /// Whether the literal lambda `argument`'s body reaches a declaration only its own class may: a
    /// private field (a property's backing field), a private function, an `invokespecial`, or a
    /// nested lambda (which may be materialized as a private method). Inlined into a regenerated
    /// object, kotlinc reaches those through synthetic accessors, which the port does not generate
    /// yet.
    pub(super) fn lambda_reaches_private_members(&self, argument: u32) -> bool {
        let (lambda, _) = self.literal_lambda(argument);
        let mut pending = vec![lambda.inline_body];
        let mut seen = HashSet::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            let private = match self.ir.expr(expression) {
                // A property's backing field is private unless it is a constant or `@JvmField`,
                // whatever the property's own visibility.
                IrExpr::GetStatic(index) | IrExpr::SetStatic { index, .. } => {
                    !self.ir.statics[*index as usize].is_const
                        && !self.ir.is_jvm_field_static(*index)
                }
                IrExpr::GetField { class, index, .. }
                | IrExpr::SetField { class, index, .. }
                | IrExpr::LateinitInitialized { class, index, .. } => {
                    self.ir.classes[*class as usize].fields[*index as usize].is_private()
                }
                IrExpr::Call { callee, .. } => {
                    matches!(callee, Callee::Special { .. })
                        || callee.source_function().is_some_and(|function| {
                            self.ir.method_visibility(function).is_private()
                        })
                }
                // A nested lambda may be materialized as a method of the caller, which the object
                // could not reach and route planning cannot tell from its shape.
                IrExpr::Lambda { .. } => true,
                _ => false,
            };
            if private {
                return true;
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }
        false
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
        let value_class_parameters = lambda
            .parameter_coercions
            .iter()
            .map(InvokeCoercion::value_class)
            .collect();
        let result_semantic = self.lambda_result_semantic(&lambda);
        let LiteralLambda {
            impl_fn,
            captures,
            inline_body,
            capture_types,
            parameter_types,
            ..
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
        // kotlinc's lambda method marks its closing brace on the return a `Unit` body falls off
        // its end into, which the inliner turns into the `nop` that keeps that line.
        if !scratch.is_dead() {
            if let Some(line) = self.lambda_fallthrough_line(impl_fn) {
                self.mark_expression_line(inline_body, line, &mut scratch);
            }
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
            let marker =
                crate::jvm::debug_local_names::spliced_lambda_marker_name(self.ir, callee, impl_fn)
                    .ok_or("a spliced lambda frame has no realized class provenance")?;
            scratch.add_local_entry(
                marker_start,
                Some(end - marker_start),
                args_size,
                &marker,
                "I",
            );
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
                value_class_parameters,
                value_class_return: self.invoke_coercion(result_semantic, result).value_class(),
                captured: 0..0,
                capture_names: self.capture_names(impl_fn, captures.len()),
            },
            captures: captures
                .iter()
                .copied()
                .zip(capture_types.iter().copied())
                .collect(),
        })
    }

    /// The closing line of the lambda `impl_fn` when its body falls off its end into the implicit
    /// return of `Unit`.
    fn lambda_fallthrough_line(&self, impl_fn: u32) -> Option<u32> {
        let body = self.ir.functions[impl_fn as usize].body?;
        let IrExpr::Block { stmts, value: None } = self.ir.expr(body) else {
            return None;
        };
        self.ir.fallthrough_return_line(*stmts.last()?)
    }

    /// The names kotlinc's `capturedVars` give the lambda `impl_fn`'s first `count` parameters, its
    /// captured values: `$x` for a captured local `x`. A captured receiver is not named yet.
    fn capture_names(&self, impl_fn: u32, count: usize) -> Vec<Option<String>> {
        let identities = self.ir.fn_params.get(&impl_fn).map(|info| &info.identities);
        (0..count)
            .map(|index| {
                let identity = identities?.get(index)?;
                match identity.role {
                    crate::ir::IrParameterRole::CapturedValue { .. } => {
                        Some(format!("${}", identity.source_name.as_ref()?))
                    }
                    _ => None,
                }
            })
            .collect()
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

/// Emits `convert` over the reference on the stack, or, when it may be null, branches around it so
/// null stays null: an instance call or a factory would throw on it or wrap it.
fn null_preserving(code: &mut CodeBuilder, nullable: bool, convert: impl FnOnce(&mut CodeBuilder)) {
    if !nullable {
        convert(code);
        return;
    }
    let null_case = code.new_label();
    let done = code.new_label();
    code.dup();
    code.ifnull(null_case);
    convert(code);
    code.goto(done);
    code.bind(null_case);
    code.pop();
    code.aconst_null();
    code.bind(done);
}
