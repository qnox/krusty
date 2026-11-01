//! A literal lambda passed to an inline function, compiled into the method node kotlinc's inliner
//! receives for it (`IrExpressionLambdaImpl`): the lambda's parameters from slot 0, the values it
//! captures after them, its `$i$a$` marker as its first local, and its body ending in a return of
//! its own type. The inliner places that node at each `invoke` of the lambda.

use super::lambda_route::SpliceReason;
use super::*;
use crate::jvm::classreader::{ExcEntry, MethodLocal};
use crate::jvm::method_node::{CodeAttribute, LabelId};
use crate::types::TyKind;

const GETSTATIC: u8 = 0xb2;
const IRETURN: u8 = 0xac;
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
    /// The checked type of the body's value.
    result_semantic: Ty,
    /// A suspend lambda returns its value as the `Object` of `invoke`, boxed within its own body.
    suspend: bool,
}

/// A callable reference the bytecode inliner can place as a lambda: its adapter returns one value.
/// The capture prefix is the reference's captures followed by its bound receiver. `None` for a
/// reference that stays a carrier.
pub(super) fn callable_reference_template(
    ir: &IrFile,
    argument: u32,
) -> Option<(u32, Vec<u32>, u32, usize, Ty)> {
    let IrExpr::CallableReference(reference) = ir.expr(argument) else {
        return None;
    };
    let (adapter, captures, inline_body, arity) =
        ir.callable_reference_inline_template(argument)?;
    Some((
        adapter,
        captures,
        inline_body,
        arity,
        reference.function_type,
    ))
}

impl Emitter<'_> {
    /// The literal lambda `argument` and its source arity, every type read from the checked IR. An
    /// error names the checked fact the literal lacks; the implementation method never stands in
    /// for it.
    fn literal_lambda(&self, argument: u32) -> Result<(LiteralLambda, usize), &'static str> {
        if let Some((impl_fn, captures, inline_body, arity, function_type)) =
            callable_reference_template(self.ir, argument)
        {
            return self.callable_reference_literal(
                impl_fn,
                captures,
                inline_body,
                arity,
                function_type,
            );
        }
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
        let (semantic, suspend) = match self
            .ir
            .logical_types
            .get(&argument)
            .map(|ty| ty.non_null())
            .map(Ty::kind)
        {
            Some(TyKind::Fun(signature)) if signature.params.len() == arity => {
                (signature.params.clone(), false)
            }
            // A suspend lambda's last parameter is the continuation its `invoke` is passed.
            Some(TyKind::Fun(signature))
                if signature.suspend && signature.params.len() + 1 == arity =>
            {
                let mut params = signature.params.clone();
                params.push(Ty::obj("kotlin/coroutines/Continuation"));
                (params, true)
            }
            _ => {
                return Err(
                    "a placed lambda has no checked function type with each argument's type",
                );
            }
        };
        let result_semantic = *self
            .ir
            .logical_types
            .get(&inline_body)
            .ok_or("a placed lambda's body has no checked result type")?;
        let (parameter_types, parameter_coercions) = physical_parameters
            .iter()
            .zip(semantic.iter().chain(std::iter::repeat(&Ty::Error)))
            .map(|(&physical, &semantic)| self.inline_parameter(semantic, physical))
            .unzip();
        Ok((
            LiteralLambda {
                impl_fn,
                captures,
                inline_body,
                capture_types: capture_types.to_vec(),
                parameter_types,
                parameter_coercions,
                result_semantic,
                suspend,
            },
            arity,
        ))
    }

    /// A callable reference as the lambda an inline call places: the adapter's returned call, with
    /// the reference's captures and bound receiver ahead of its function parameters.
    fn callable_reference_literal(
        &self,
        impl_fn: u32,
        captures: Vec<u32>,
        inline_body: u32,
        arity: usize,
        function_type: Ty,
    ) -> Result<(LiteralLambda, usize), &'static str> {
        let TyKind::Fun(signature) = function_type.non_null().kind() else {
            return Err("a callable reference has no function type");
        };
        let physical = jvm_function_params(self.ir, impl_fn);
        let split = captures.len();
        if physical.len() != split + arity {
            return Err("a callable reference adapter does not take its captures and parameters");
        }
        let (capture_types, physical_parameters) = physical.split_at(split);
        // A suspend reference's adapter, like a suspend literal's implementation, takes the
        // continuation its `invoke` is passed after the function type's parameters.
        let mut semantic = signature.params.clone();
        if signature.suspend {
            semantic.push(Ty::obj("kotlin/coroutines/Continuation"));
        }
        let result_semantic = self
            .ir
            .logical_types
            .get(&inline_body)
            .copied()
            .unwrap_or(signature.ret);
        let (parameter_types, parameter_coercions) = physical_parameters
            .iter()
            .zip(semantic.iter())
            .map(|(&physical, &semantic)| self.inline_parameter(semantic, physical))
            .unzip();
        Ok((
            LiteralLambda {
                impl_fn,
                captures,
                inline_body,
                capture_types: capture_types.to_vec(),
                parameter_types,
                parameter_coercions,
                result_semantic,
                suspend: signature.suspend,
            },
            arity,
        ))
    }

    /// Why the call passing the literal lambda `argument` takes the splice route, if it must: the
    /// lambda shapes the MethodInliner port does not own yet. Decided from the checked IR before
    /// anything of the call is emitted.
    pub(super) fn lambda_splice_reason(
        &mut self,
        argument: u32,
    ) -> Result<Option<SpliceReason>, &'static str> {
        let (lambda, arity) = self.literal_lambda(argument)?;
        // A suspension in the lambda needs the splice's state-machine markers until the coroutine
        // transformer runs on inlined bytecode; a suspend lambda's `Continuation` parameter is the
        // one its arity does not count.
        if self.suspends_within(lambda.inline_body) || lambda.parameter_types.len() != arity {
            return Ok(Some(SpliceReason::SuspendingLambda));
        }
        self.ir
            .lambda_exit(lambda.inline_body)
            .ok_or("an inline lambda reached emission without its recorded non-local exit")?;
        if lambda
            .parameter_coercions
            .iter()
            .any(|coercion| matches!(coercion, InvokeCoercion::Unported))
        {
            return Ok(Some(SpliceReason::ValueClassAdapter));
        }
        let declared_result = self.ir.functions[lambda.impl_fn as usize].ret;
        let result_semantic = lambda.result_semantic;
        if declared_result.is_jvm_scalar()
            && semantic_scalar_adapter(result_semantic, declared_result) != declared_result
        {
            return Ok(Some(SpliceReason::ValueClassAdapter));
        }
        let frame = lambda_frame(&lambda.capture_types, &lambda.parameter_types);
        let result = self.inline_body_result_ty(lambda.inline_body, &frame.slots);
        if matches!(
            self.invoke_coercion(result_semantic, result),
            InvokeCoercion::Unported
        ) {
            return Ok(Some(SpliceReason::ValueClassAdapter));
        }
        Ok(None)
    }

    /// Whether the literal lambda `argument` leaves by a `break` or `continue` of a loop outside
    /// it or by a return of an inline expansion outside it, which no real call of its function
    /// could carry out.
    pub(in crate::jvm::ir_emit) fn literal_jumps_out(&self, argument: u32) -> bool {
        self.literal_lambda(argument).is_ok_and(|(lambda, _)| {
            self.ir
                .lambda_exit(lambda.inline_body)
                .is_some_and(|exit| exit.jumps || exit.frame_returns)
        })
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
            .and_then(|class| crate::jvm::value_classes::boxed_value_class_carrier(self.ir, class));
        let carrier = unboxed.map_or(physical, |carrier| ir_ty_to_jvm(&carrier));
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

    /// Unbox a captured value of value-class type `evaluated` (a boxed receiver after `!!`) into
    /// `carrier`, the unboxed representation the lambda's frame captures it as.
    pub(super) fn unbox_captured_value_class(
        &mut self,
        evaluated: Ty,
        carrier: Ty,
        code: &mut CodeBuilder,
    ) {
        let Some(class) = self.unboxed_value_class(evaluated, carrier) else {
            return;
        };
        if evaluated.is_nullable() {
            return;
        }
        let unbox = self.cw.methodref(
            &class,
            "unbox-impl",
            &format!("(){}", type_descriptor(carrier)),
        );
        code.invokevirtual(unbox, 0, i32::from(slot_words(carrier)));
    }

    /// The value class of Kotlin type `semantic` when `carrier` is its unboxed representation.
    fn unboxed_value_class(&self, semantic: Ty, carrier: Ty) -> Option<String> {
        let class = semantic.non_null().obj_internal()?;
        if !self.is_value_class_ty(&semantic) {
            return None;
        }
        let unboxed = crate::jvm::value_classes::boxed_value_class_carrier(self.ir, class)?;
        let carried = type_descriptor(carrier);
        (carried != type_descriptor(Ty::obj_name(class))
            && carried == type_descriptor(ir_ty_to_jvm(&unboxed)))
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
    pub(super) fn lambda_shape(
        &self,
        argument: u32,
    ) -> Result<(inliner::Lambda, Vec<(u32, Ty)>), &'static str> {
        let (lambda, _) = self.literal_lambda(argument)?;
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
            capture_names: self.capture_names(argument, lambda.impl_fn, lambda.captures.len()),
            default_class: None,
            suspend: lambda.suspend,
        };
        let captures = lambda
            .captures
            .iter()
            .copied()
            .zip(lambda.capture_types.iter().copied())
            .collect();
        Ok((shape, captures))
    }

    /// Whether the literal lambda `argument`'s body reaches a declaration only its own class may: a
    /// private field (a property's backing field), a private function, an `invokespecial`, or a
    /// nested lambda (which may be materialized as a private method). Inlined into a regenerated
    /// object, kotlinc reaches those through synthetic accessors, which the port does not generate
    /// yet.
    pub(super) fn lambda_reaches_private_members(
        &self,
        argument: u32,
    ) -> Result<bool, &'static str> {
        let (lambda, _) = self.literal_lambda(argument)?;
        let mut pending = vec![lambda.inline_body];
        let mut seen = HashSet::default();
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
                    matches!(&**callee, Callee::Special { .. })
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
                return Ok(true);
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }
        Ok(false)
    }

    /// Whether every value the lambda `argument` captures can be bound: kotlinc binds a captured
    /// caller local to its slot only without a cast, and evaluates any other value into a
    /// temporary.
    pub(super) fn lambda_captures_bind(&self, argument: u32) -> Result<bool, &'static str> {
        let (lambda, _) = self.literal_lambda(argument)?;
        Ok(lambda
            .captures
            .iter()
            .zip(&lambda.capture_types)
            .all(|(&capture, &ty)| {
                !self.captures_caller_local(capture)
                    || !matches!(
                        self.caller_local_binding(capture, ty),
                        Binding::CallerLocal {
                            checkcast: Some(_),
                            ..
                        }
                    )
            }))
    }

    /// Compile the literal lambda `argument` of a call to the inline function `callee`, which route
    /// planning gave to the MethodInliner port ([`Self::lambda_splice_reason`] found no reason to
    /// splice it). An error is a broken invariant of that plan, never a cue to try another route.
    pub(super) fn inline_lambda_node(
        &mut self,
        argument: u32,
        callee: &str,
    ) -> Result<LambdaArgument, &'static str> {
        let reference_line = matches!(self.ir.expr(argument), IrExpr::CallableReference(_))
            .then(|| self.ir.expr_source_lines.get(&argument).copied())
            .flatten();
        let (lambda, _) = self.literal_lambda(argument)?;
        let value_class_parameters = lambda
            .parameter_coercions
            .iter()
            .map(InvokeCoercion::value_class)
            .collect();
        let result_semantic = lambda.result_semantic;
        let LiteralLambda {
            impl_fn,
            captures,
            inline_body,
            capture_types,
            parameter_types,
            suspend,
            ..
        } = lambda;
        let LambdaFrame {
            slots: parameter_slots,
            mut args_size,
        } = lambda_frame(&capture_types, &parameter_types);
        let declared_result = self.ir.functions[impl_fn as usize].ret;
        // A suspension in the body passes on the caller's continuation, which the lambda captures
        // as its last value, as kotlinc's lambda captures `$completion`. A suspend lambda passes on
        // its own `$completion`, its last parameter.
        let continuation_read = if suspend {
            None
        } else {
            self.continuation_read_within(inline_body)
        };
        let placed_machine = self
            .emit_time_machines
            .transformed(impl_fn)
            .filter(|_| suspend)
            .zip(parameter_slots.last().map(|&(slot, _)| slot));
        let continuation_slot = continuation_read.map(|_| {
            let slot = args_size;
            args_size += 1;
            slot
        });
        let own_captures = captures.len();
        let mut capture_types = capture_types;
        let mut captures = captures;
        if let Some(read) = continuation_read {
            capture_types.push(Ty::obj("kotlin/coroutines/Continuation"));
            captures.push(read);
        }

        // The lambda's node is a method of its own: its body's locals are laid out in a frame of
        // their own, above its parameters, captures and marker.
        let caller_frame = std::mem::take(&mut self.frame);
        // Its body is the method's outermost scope: the locals declared directly in it stay open
        // to the method's end, as a function's own locals do, and only nested blocks close theirs.
        let caller_depth = std::mem::replace(&mut self.block_depth, 0);
        let caller_locals = std::mem::take(&mut self.open_locals);
        // A non-local `return` in the body leaves only the body's own `try`s here. The caller's
        // `finally` blocks run ahead of it where the inlined code is written into the caller, and
        // the caller's protected regions are ranges of the caller's code, not of this node.
        let caller_finalizers = std::mem::take(&mut self.return_finalizers);
        let caller_regions = std::mem::take(&mut self.protected_regions);
        // A `break` or `continue` of the caller's loop leaves the body by a marked return.
        let caller_loops = self.enter_lambda_loops();
        // The scratch lambda is a separate method with its own inline-frame identity space.
        let caller_next_inline_frame_identity =
            std::mem::replace(&mut self.next_inline_frame_identity, 0);
        // It numbers its own inline scopes, as kotlinc's lambda method has a codegen of its own.
        let caller_inline_scopes = self.inline_scopes;
        self.inline_scopes = caller_inline_scopes.map(|_| Default::default());
        let caller_inline_scope_units = std::mem::take(&mut self.inline_scope_units);
        let states_before = self.machine_next_ordinal;
        let caller_continuation = match continuation_slot {
            Some(slot) => self.continuation_slot.replace(slot),
            None => self.continuation_slot,
        };
        self.frame.reserve_through(args_size + 1);
        let mut scratch = CodeBuilder::new(args_size);
        scratch.push_int(0, self.cw);
        store(Ty::Int, args_size, &mut scratch);
        if let Some(line) = reference_line {
            self.mark_expression_line(argument, line, &mut scratch);
        }
        let marker_start = u16::try_from(scratch.bytes.len())
            .map_err(|_| "an inline lambda's code is too long")?;
        let caller_machine = placed_machine
            .map(|(machine, completion)| self.arm_placed_suspend_lambda(machine, completion));
        let caller_type_parameters = self.type_parameter_function.replace(impl_fn);
        let result = self.emit_fn_body_inline(inline_body, &parameter_slots, &mut scratch);
        self.type_parameter_function = caller_type_parameters;
        if let Some(caller_machine) = caller_machine {
            self.disarm_placed_suspend_lambda(caller_machine);
        }
        self.return_finalizers = caller_finalizers;
        self.protected_regions = caller_regions;
        let jumps = self.leave_lambda_loops(caller_loops);
        self.frame = caller_frame;
        self.block_depth = caller_depth;
        let body_locals = std::mem::replace(&mut self.open_locals, caller_locals);
        self.next_inline_frame_identity = caller_next_inline_frame_identity;
        self.inline_scopes = caller_inline_scopes;
        self.inline_scope_units = caller_inline_scope_units;
        self.continuation_slot = caller_continuation;
        if self.machine_next_ordinal != states_before {
            return Err("a lambda planned for the inliner started a suspension");
        }
        // kotlinc's lambda method marks its closing brace on the return a `Unit` body falls off
        // its end into, which the inliner turns into the `nop` that keeps that line. A value
        // template's distinct evaluated-result end is already attached to its exact expression
        // in common IR, so it was emitted on the successful path before any local-return join.
        if !scratch.is_dead() {
            if let Some(line) = self.lambda_fallthrough_line(impl_fn) {
                self.mark_expression_line(inline_body, line, &mut scratch);
            }
        }
        // A suspend lambda's `invoke` returns `Object`: its body boxes a primitive value and returns
        // `Unit.INSTANCE` for a `Unit` one, before its frame ends.
        let result = if suspend && !scratch.is_dead() {
            match type_descriptor(result).as_str() {
                "V" if scratch.stack_height() == 0 => {
                    let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                    scratch.getstatic(unit, 1);
                }
                "V" => {}
                _ => super::super::scalar_coercion::box_prim_free(self.cw, &mut scratch, result),
            }
            Ty::obj("java/lang/Object")
        } else {
            result
        };
        // A `Unit` body may or may not leave `Unit.INSTANCE`; any other body leaves its value. A
        // body that always throws or returns non-locally ends without a return of its own.
        let own_return = scratch.bytes.len();
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
        // kotlinc closes the outermost scope at the end of the lambda's method, after its return:
        // the marker it declared first, then the body's own locals in declaration order, then the
        // parameters. A nested block's locals closed earlier, where the block ended.
        if self.record_locals {
            let end = u16::try_from(scratch.bytes.len())
                .map_err(|_| "an inline lambda's code is too long")?;
            let marker =
                crate::jvm::debug_local_names::spliced_lambda_marker_name(self.ir, callee, impl_fn)
                    .or_else(|| {
                        let class = crate::jvm::local_class_names::callable_reference_name(
                            self.ir, argument,
                        )?;
                        let class = class.segment_ref();
                        (!class.is_empty()).then(|| format!("$i$a$-{callee}-{class}"))
                    })
                    .ok_or("a spliced lambda frame has no realized class provenance")?;
            scratch.add_local_entry(
                marker_start,
                Some(end - marker_start),
                args_size,
                &marker,
                "I",
            );
            for local in body_locals {
                let length = end.saturating_sub(local.start);
                local.record(Some(length), &mut scratch);
            }
            let identities = self.ir.fn_params.get(&impl_fn).map(|info| &info.identities);
            let reference_parameters = callable_reference_template(self.ir, argument).is_some();
            for (index, &ty) in parameter_types.iter().enumerate() {
                // A suspend reference's adapter takes its continuation last, as a suspend
                // literal's implementation does, and kotlinc names it the same.
                let continuation = suspend && index + 1 == parameter_types.len();
                let name = if reference_parameters && continuation {
                    crate::jvm::parameter_names::local_variable(
                        &crate::ir::IrParameterIdentity::generated(
                            crate::ir::IrGeneratedParameterRole::Continuation,
                            None,
                        ),
                        "",
                    )
                } else if reference_parameters {
                    let ordinal = u32::try_from(index)
                        .map_err(|_| "a callable reference has too many parameters")?;
                    crate::jvm::debug_local_names::render(
                        self.ir,
                        None,
                        Some(
                            crate::ir::IrDebugLocalProvenance::InlineCallableReferenceParameter {
                                ordinal,
                            },
                        ),
                    )
                } else {
                    identities
                        .and_then(|identities| identities.get(own_captures + index))
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
                            _ => crate::jvm::parameter_names::local_variable(identity, ""),
                        })
                };
                if let Some(name) = name {
                    let (slot, _) = parameter_slots[own_captures + index];
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
        mark_non_local_returns(&mut node, &scratch.bytes, own_return, &jumps)?;
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
                capture_names: {
                    let mut names = self.capture_names(argument, impl_fn, own_captures);
                    if continuation_read.is_some() {
                        names.push(Some("$completion".to_string()));
                    }
                    names
                },
                default_class: None,
                suspend,
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
    /// captured values: `$x` for a captured local `x`. A callable reference `argument`'s bound
    /// receiver, which its template places after the reference's captures, is `receiver`, the
    /// value kotlinc's `InlineCallableReferenceToLambda` binds it to. A captured receiver of a
    /// literal is not named yet.
    fn capture_names(&self, argument: u32, impl_fn: u32, count: usize) -> Vec<Option<String>> {
        let identities = self.ir.fn_params.get(&impl_fn).map(|info| &info.identities);
        let bound_receiver = match self.ir.expr(argument) {
            IrExpr::CallableReference(reference) => {
                reference.bound_receiver.map(|_| reference.captures.len())
            }
            _ => None,
        };
        (0..count)
            .map(|index| {
                if bound_receiver == Some(index) {
                    return Some("receiver".to_string());
                }
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

    /// A read of the current function's own continuation in `root`'s subtree, which a lambda placed
    /// in that function captures.
    fn continuation_read_within(&self, root: u32) -> Option<u32> {
        self.continuation_slot?;
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if matches!(self.ir.expr(expression), IrExpr::CurrentContinuation)
                && !self.is_fake_continuation(expression)
            {
                return Some(expression);
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }
        None
    }

    /// Whether `root`'s subtree holds one of the current function's suspension points.
    fn suspends_within(&self, root: u32) -> bool {
        if self.machine_suspensions.is_empty() {
            return false;
        }
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if self.machine_suspensions.contains_key(&expression) {
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
            .named_local_entries()
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

/// Mark each return of the lambda's `node`, read from `code`, except its own return at `own_return`:
/// the body lowers a local return to its value, so every other return leaves the function the
/// lambda is written in (kotlinc's `generateGlobalReturnFlag`), or the loop `jumps` names for the
/// return at its offset.
fn mark_non_local_returns(
    node: &mut MethodNode,
    code: &[u8],
    own_return: usize,
    jumps: &[(usize, String)],
) -> Result<(), &'static str> {
    let mut offsets = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        offsets.push(pc);
        pc += crate::jvm::bytecode::instruction_len(code, pc)
            .ok_or("an inline lambda's code has a malformed instruction")?;
    }
    let mut offsets = offsets.into_iter();
    let mut at = 0;
    while at < node.nodes.len() {
        if let Node::Insn(insn) = &node.nodes[at] {
            let offset = offsets
                .next()
                .ok_or("an inline lambda's node has more instructions than its code")?;
            if matches!(insn, Insn::Op(IRETURN..=RETURN)) && offset != own_return {
                let name = jumps
                    .iter()
                    .find(|(jump, _)| *jump == offset)
                    .map_or_else(String::new, |(_, name)| name.clone());
                node.nodes
                    .insert(at, inliner::non_local_return_marker(name));
                at += 1;
            }
        }
        at += 1;
    }
    Ok(())
}

/// A `Unit` lambda: krusty's body yields `Unit.INSTANCE` for the `Object` `invoke` returns, where
/// kotlinc's lambda returns `void` and the inliner loads `Unit.INSTANCE` after it. When every
/// return is such a load, the lambda is made to return `void`. The load may precede the closing
/// brace's line, which kotlinc places on the `void` return itself; a label no instruction jumps to
/// carries only that line.
fn return_unit_as_void(node: &mut MethodNode, return_type: String) -> String {
    if return_type != "Lkotlin/Unit;" {
        return return_type;
    }
    let loads_unit = |node: &Node| {
        matches!(node, Node::Insn(Insn::Field { op: GETSTATIC, owner, name, desc })
            if owner == "kotlin/Unit" && name == "INSTANCE" && desc == "Lkotlin/Unit;")
    };
    let targets: HashSet<LabelId> = node
        .nodes
        .iter()
        .filter_map(|entry| match entry {
            Node::Insn(insn) => Some(insn.jump_targets()),
            _ => None,
        })
        .flatten()
        .chain(
            node.try_catch_blocks
                .iter()
                .flat_map(|block| [block.start, block.end, block.handler]),
        )
        .collect();
    let debug_only = |entry: &Node| match entry {
        Node::Line { .. } => true,
        Node::Label(label) => !targets.contains(label),
        Node::Insn(_) => false,
    };
    let mut loads = Vec::new();
    for (at, entry) in node.nodes.iter().enumerate() {
        let marked = at
            .checked_sub(1)
            .is_some_and(|before| inliner::is_non_local_return_marker(&node.nodes[before]));
        if marked || !matches!(entry, Node::Insn(Insn::Op(ARETURN))) {
            continue;
        }
        let load = node.nodes[..at]
            .iter()
            .rposition(|entry| !debug_only(entry));
        match load {
            Some(load) if loads_unit(&node.nodes[load]) => loads.push((load, at)),
            _ => return return_type,
        }
    }
    for &(load, at) in loads.iter().rev() {
        node.nodes[at] = Node::Insn(Insn::Op(RETURN));
        node.nodes.remove(load);
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
