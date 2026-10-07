//! Materialize checked semantic values at JVM consumption boundaries.
//!
//! Common IR has already selected the callable, argument mapping, and semantic types. This module
//! owns only the physical JVM transition applied immediately after an operand is emitted: scalar
//! boxing/unboxing, value-class carriers, numeric conversion, and reference stack coercion.

use super::*;

/// An operand whose value arrives in an erased reference slot before checked IR narrows it.
pub(super) enum ErasedResult {
    /// A call's erased result under the implicit coercion to its substituted type.
    Coerced { call: crate::ir::ExprId, slot: Ty },
    /// A function value's `invoke`, whose result is `Object`.
    Invocation,
}

/// How a reference value reaches a consumer's reference type.
pub(super) enum ReferenceCoercion {
    /// The value is already acceptable; the verifier keeps its current type.
    Unchanged,
    /// A `checkcast` to this internal name.
    Cast(String),
    /// A value-class or unsigned carrier, adapted by `narrow_on_stack`.
    Carried,
}

impl Emitter<'_> {
    /// Emit `expression` and materialize it at the consumer's declared type.
    pub(super) fn emit_value_as(
        &mut self,
        expression: crate::ir::ExprId,
        expected: Ty,
        code: &mut CodeBuilder,
    ) {
        self.emit_value(expression, code);
        let source = self.value_ty(expression);
        let target = ir_ty_to_jvm(&expected);
        // A type-parameter local kept in its erased reference slot is read here as the
        // primitive the caller returns. The store boxed into that slot; this is the unbox.
        if source.is_reference() && target.is_jvm_scalar() {
            unbox_prim_from(
                self.cw,
                code,
                source,
                semantic_scalar_adapter(expected, target),
            );
            return;
        }
        self.coerce_reference_on_stack(source, expected, code);
    }

    /// Emit an operand whose consumer materializes it at its own slot type, answering the stack type
    /// left for that materialization. An erased reference result stays erased here: kotlinc narrows
    /// it only when the consumer's type asks for it, not to the substituted type.
    pub(super) fn emit_consumed_operand(
        &mut self,
        expression: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) -> Ty {
        if let IrExpr::Vararg {
            array_type,
            elements,
            spreads,
        } = self.ir.expr(expression).clone()
        {
            if let Some(physical) = super::vararg::emit_consumed_reference_copy(
                self,
                &array_type,
                &elements,
                &spreads,
                code,
            ) {
                return physical;
            }
        }
        match self.erased_reference_result(expression) {
            Some(ErasedResult::Coerced { call, slot }) => {
                self.emit_value(call, code);
                slot
            }
            Some(ErasedResult::Invocation) => {
                // Scoped to this one emission: a copy of the node emitted elsewhere (a `finally`
                // body) narrows as usual.
                self.erased_invocations.insert(expression);
                self.emit_value(expression, code);
                self.erased_invocations.remove(&expression);
                Ty::obj("java/lang/Object")
            }
            None => {
                self.emit_value(expression, code);
                self.value_ty(expression)
            }
        }
    }

    /// How `expression` produces a reference value in an erased `Object` slot that checked IR
    /// narrows to another JVM reference type: a generic declaration's result (from a source or provider
    /// callee) under its implicit coercion, or a function value's `invoke`. Carriers (value
    /// classes, unsigned, primitives) keep their own adaptation.
    pub(super) fn erased_reference_result(
        &self,
        expression: crate::ir::ExprId,
    ) -> Option<ErasedResult> {
        let narrows = |slot: Ty, target: Ty| {
            ir_ty_to_jvm(&slot).is_reference()
                && ir_ty_to_jvm(&target).is_reference()
                && matches!(
                    self.reference_coercion(slot, target),
                    ReferenceCoercion::Cast(_)
                )
        };
        match self.ir.expr(expression) {
            crate::ir::IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => {
                // Only a slot erased to `Object`: a bounded type parameter's slot (`T : Base`) is
                // already a class a receiver or argument adapter takes as the value's own type.
                let slot = *self.ir.physical_types.get(arg)?;
                (jvm_is_erased_top(ir_ty_to_jvm(&slot)) && narrows(slot, *type_operand))
                    .then_some(ErasedResult::Coerced { call: *arg, slot })
            }
            crate::ir::IrExpr::InvokeFunction { ret, .. } => {
                (!matches!(ret, Ty::Unit | Ty::Nothing)
                    && narrows(Ty::obj("java/lang/Object"), *ret))
                .then_some(ErasedResult::Invocation)
            }
            _ => None,
        }
    }

    /// The call and erased `Object` slot of a generic result that checked IR narrows to a scalar
    /// (`Holder<Int>.get()` read as `Int`). Its value is already the box a reference consumer takes.
    pub(super) fn erased_scalar_result(
        &self,
        expression: crate::ir::ExprId,
    ) -> Option<(crate::ir::ExprId, Ty)> {
        let crate::ir::IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = self.ir.expr(expression)
        else {
            return None;
        };
        // The coroutine transformer closes a suspension point at its declared result before the
        // comparison consumes it. A primitive result is therefore already unboxed here even
        // though the call's original descriptor returned `Object`; preserving that erased slot
        // would make `areEqual(Object, Object)` consume a scalar.
        if self.transformed_result(*arg).is_some() {
            return None;
        }
        let slot = *self.ir.physical_types.get(arg)?;
        (jvm_is_erased_top(ir_ty_to_jvm(&slot)) && ir_ty_to_jvm(type_operand).is_jvm_scalar())
            .then_some((*arg, slot))
    }

    /// Whether checked primitive equality consumes an ordinary JVM wrapper retained by an exact
    /// value-class underlying-property read. The frontend's equality mode remains authoritative;
    /// this is only the JVM representation choice for those already-selected operands. An
    /// ordinary generic call result and an unsigned/value-class box must be unboxed instead, even
    /// though all three producers have an erased `Object` descriptor.
    pub(super) fn primitive_equality_uses_erased_wrapper(
        &self,
        mode: Option<crate::ir::EqualityMode>,
        lhs: crate::ir::ExprId,
        rhs: crate::ir::ExprId,
    ) -> bool {
        mode == Some(crate::ir::EqualityMode::Primitive)
            && [lhs, rhs].into_iter().any(|operand| {
                self.erased_scalar_result(operand).is_some()
                    && self.erased_wrapper_property_coercion(operand)
            })
    }

    /// Whether an implicit-coercion chain contains the exact generic-property read that retained
    /// an ordinary JVM wrapper. Checked lowering can add one coercion and value-class realization
    /// another; neither changes which recorded producer occupies the erased slot.
    fn erased_wrapper_property_coercion(&self, mut expression: crate::ir::ExprId) -> bool {
        loop {
            if self
                .ir
                .jvm_erased_primitive_wrapper_values
                .contains(&expression)
            {
                return true;
            }
            match self.ir.expr(expression) {
                crate::ir::IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => expression = *arg,
                _ => return false,
            }
        }
    }

    /// Whether `ty` names a `@JvmInline value class`, whose values use a backend-owned carrier.
    pub(super) fn is_value_class_ty(&self, ty: &Ty) -> bool {
        ty.non_null().obj_internal().is_some_and(|fq_name| {
            crate::jvm::value_classes::is_boxed_value_class(self.ir, fq_name)
        })
    }

    /// Narrow an erased reference on top of the stack to its consumption type.
    pub(super) fn narrow_on_stack(&mut self, source: Ty, expected: Ty, code: &mut CodeBuilder) {
        let source = ir_ty_to_jvm(&source);
        if !jvm_is_erased_top(source) {
            return;
        }
        let expected = ir_ty_to_jvm(&expected);
        if !expected.is_reference() || crate::jvm::names::same_type_descriptor(source, expected) {
            return;
        }
        let internal = crate::jvm::names::instanceof_internal_name(expected);
        if internal != "java/lang/Object" {
            let class = self.cw.class_ref(&internal);
            code.checkcast(class);
        }
    }

    /// Adapt one semantic value to the physical slot named by a JVM descriptor. Wrapper identity
    /// and primitive carriers are backend facts; core has already selected the callable.
    pub(super) fn adapt_physical_operand(
        &mut self,
        source: Ty,
        semantic: Ty,
        destination_semantic: Option<Ty>,
        physical: Ty,
        code: &mut CodeBuilder,
    ) {
        // `source` comes from `value_ty`/a spill slot and is already the verifier-visible stack type.
        // Re-running semantic erasure here would turn the boxed `Obj("kotlin/Int")` back into scalar
        // `Int` and box it a second time.
        let source_jvm = source;
        crate::trace_compiler!(
            "value_classes",
            "descriptor operand source={source:?} semantic={semantic:?} jvm={source_jvm:?} physical={physical:?}"
        );
        if self.materialize_unit(source_jvm, physical, code) {
            return;
        }
        // A checked value-class value can reach a physical carrier slot as its BOX object (for
        // example, an element read from `Collection<Item>` inside an inlined `all` lambda). Primitive
        // wrapper unboxing is not applicable: `Item` is not `Integer`, even when its carrier is `int`.
        let boxed_value_class = semantic.non_null().obj_internal().filter(|classifier| {
            source_jvm.non_null().obj_internal() == Some(*classifier)
                && self.is_value_class_ty(&semantic)
        });
        let underlying = boxed_value_class.and_then(|classifier| {
            crate::jvm::value_classes::boxed_value_class_underlying(self.ir, classifier)
        });
        if let (Some(classifier), Some(underlying)) = (boxed_value_class, underlying) {
            let carrier = jvm_declared_ty(&underlying);
            // A reference supertype/generic descriptor consumes the BOX itself. Unbox only when the
            // selected descriptor consumes this value class's actual carrier.
            // A nullable destination over a carrier that has a null of its own is the box too.
            let destination_is_concrete_value_class =
                destination_semantic.is_none_or(|destination| {
                    destination.non_null().obj_internal() == Some(classifier)
                        && !(destination.is_nullable() && underlying.is_nullable())
                });
            // The box and the carrier of a primitive-array value class share a descriptor.
            // A physical stamp that still names the value class is the box; unboxing it deletes
            // a `box-impl` just emitted for a supertype coercion.
            let physical_is_box = physical.non_null().obj_internal() == Some(classifier);
            if destination_is_concrete_value_class
                && !physical_is_box
                && crate::jvm::names::same_type_descriptor(carrier, physical)
            {
                let descriptor = format!("(){}", type_descriptor(carrier));
                let method = self
                    .cw
                    .methodref(&classifier.render(), "unbox-impl", &descriptor);
                code.invokevirtual(method, 0, slot_words(carrier) as i32);
            }
            return;
        }
        if source_jvm.is_jvm_scalar() && physical.is_reference() {
            let semantic = if physical.non_null().is_unsigned() {
                physical.non_null()
            } else {
                semantic
            };
            box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, source_jvm));
        } else if source_jvm.is_reference() && physical.is_jvm_scalar() {
            unbox_prim_from(
                self.cw,
                code,
                source_jvm,
                semantic_scalar_adapter(semantic, physical),
            );
        } else if source_jvm.is_jvm_scalar() && physical.is_jvm_scalar() {
            emit_num_conv(source_jvm, physical, code);
        } else if source_jvm.is_reference() && physical.is_reference() {
            self.coerce_reference_on_stack(source_jvm, physical, code);
        }
    }

    /// kotlinc's `StackValue.coerce` between two references: materialize a value at the consumer's
    /// reference type whether that narrows erased `Object` or widens a subtype. `Object`, null, and a
    /// diverging value need no cast.
    pub(super) fn coerce_reference_on_stack(
        &mut self,
        source: Ty,
        target: Ty,
        code: &mut CodeBuilder,
    ) {
        if self.materialize_unit(source, ir_ty_to_jvm(&target), code) {
            return;
        }
        match self.reference_coercion(source, target) {
            ReferenceCoercion::Unchanged => {}
            ReferenceCoercion::Cast(internal) => {
                let class = self.cw.class_ref(&internal);
                code.checkcast(class);
            }
            ReferenceCoercion::Carried => self.narrow_on_stack(source, target, code),
        }
    }

    /// A Unit-valued operation leaves nothing on the stack (`V`). A reference consumer receives the
    /// `kotlin/Unit` singleton, as kotlinc's `StackValue.coerce` from `VOID_TYPE` does. Answers
    /// whether it pushed it.
    fn materialize_unit(&mut self, source: Ty, physical: Ty, code: &mut CodeBuilder) -> bool {
        if source != Ty::Unit || !physical.is_reference() {
            return false;
        }
        let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        code.getstatic(unit, 1);
        true
    }

    /// What `coerce_reference_on_stack` does to a `source` value consumed as `target`, so a
    /// pre-emission fact about the value's verifier type can follow the same decision.
    pub(super) fn reference_coercion(&self, source: Ty, target: Ty) -> ReferenceCoercion {
        if matches!(source.non_null(), Ty::Null | Ty::Nothing) {
            return ReferenceCoercion::Unchanged;
        }
        // A value class or unsigned type is carried as its underlying value, which the erased type
        // does not name. Its representation belongs to the value-class adapter, so only a value
        // coming out of erased `Object` is narrowed here.
        let carried = |mut ty: Ty| loop {
            ty = ty.non_null();
            if ty.is_unsigned() || self.is_value_class_ty(&ty) {
                break true;
            }
            match ty.array_elem() {
                Some(element) => ty = element,
                None => break false,
            }
        };
        if carried(source) || carried(target) {
            return ReferenceCoercion::Carried;
        }
        let (from, to) = (ir_ty_to_jvm(&source), ir_ty_to_jvm(&target));
        let (from_descriptor, to_descriptor) = (type_descriptor(from), type_descriptor(to));
        if !from.is_reference() || !to.is_reference() || from_descriptor == to_descriptor {
            return ReferenceCoercion::Unchanged;
        }
        // Between arrays of the same dimension count, an `Object` element needs no cast.
        let dimensions = |descriptor: &str| descriptor.bytes().take_while(|&b| b == b'[').count();
        if dimensions(&from_descriptor) > 0
            && dimensions(&from_descriptor) == dimensions(&to_descriptor)
            && to_descriptor[dimensions(&to_descriptor)..] == *"Ljava/lang/Object;"
        {
            return ReferenceCoercion::Unchanged;
        }
        let internal = crate::jvm::names::instanceof_internal_name(to);
        if internal == "java/lang/Object" {
            ReferenceCoercion::Unchanged
        } else {
            ReferenceCoercion::Cast(internal)
        }
    }

    pub(super) fn adapt_physical_operand_for(
        &mut self,
        expression: crate::ir::ExprId,
        source: Ty,
        physical: Ty,
        code: &mut CodeBuilder,
    ) {
        let semantic = self
            .ir
            .logical_types
            .get(&expression)
            .copied()
            .unwrap_or(source);
        self.adapt_physical_operand(source, semantic, None, physical, code);
    }

    pub(super) fn adapt_physical_call_operand_for(
        &mut self,
        call_expression: crate::ir::ExprId,
        parameter_index: usize,
        expression: crate::ir::ExprId,
        source: Ty,
        physical: Ty,
        code: &mut CodeBuilder,
    ) {
        if self
            .default_call_operands
            .is_continuation(call_expression, parameter_index)
        {
            return;
        }
        let semantic = self
            .ir
            .logical_types
            .get(&expression)
            .copied()
            .unwrap_or(source);
        let destination_semantic = self
            .ir
            .call_declared_params
            .get(&call_expression)
            .and_then(|parameters| parameters.get(parameter_index))
            .copied();
        self.adapt_physical_operand(source, semantic, destination_semantic, physical, code);
    }

    pub(super) fn adapt_physical_constructor_operand_for(
        &mut self,
        construction: crate::ir::ExprId,
        parameter_index: usize,
        expression: crate::ir::ExprId,
        source: Ty,
        physical: Ty,
        code: &mut CodeBuilder,
    ) {
        let semantic = self
            .ir
            .logical_types
            .get(&expression)
            .copied()
            .unwrap_or(source);
        let destination_semantic = self
            .ir
            .construction_declared_params
            .get(&construction)
            .and_then(|parameters| parameters.get(parameter_index))
            .copied();
        self.adapt_physical_operand(source, semantic, destination_semantic, physical, code);
    }

    pub(super) fn constructor_physical_params(
        &self,
        construction: crate::ir::ExprId,
        descriptor: &str,
    ) -> Vec<Ty> {
        let checked = self
            .ir
            .construction_declared_params
            .get(&construction)
            .map(|params| params.as_ref());
        crate::jvm::physical_type::constructor_operand_tys(descriptor, checked)
            .expect("constructor descriptor must be valid")
    }
}
