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
    /// Read a boxed mutable local at either its declared value type or its holder field type.
    ///
    /// Ordinary consumers need the `ObjectRef.element` value narrowed to the checked reference
    /// type. A reference comparison accepts the `Object` slot directly, exactly as `if_acmp*` does,
    /// and must not introduce a checkcast that cannot affect reference identity.
    pub(super) fn emit_shared_cell_read(
        &mut self,
        holder: crate::ir::ExprId,
        elem: Ty,
        materialize: bool,
        code: &mut CodeBuilder,
    ) -> Ty {
        self.emit_value(holder, code);
        let (class, descriptor) = ref_class(&elem);
        let field = self.cw.fieldref(class, "element", descriptor);
        let slot = crate::jvm::physical_type::descriptor_operand_ty(descriptor);
        code.getfield(field, slot_words(slot) as i32);
        if materialize {
            self.narrow_shared_cell_element(elem, code)
        } else {
            slot
        }
    }

    /// Narrow a shared cell's `element`, read in its holder field's slot, to the checked element
    /// type, answering the resulting stack type.
    pub(super) fn narrow_shared_cell_element(&mut self, elem: Ty, code: &mut CodeBuilder) -> Ty {
        let value = ir_ty_to_jvm(&elem);
        if value.is_reference()
            && crate::jvm::names::instanceof_internal_name(value) != "java/lang/Object"
        {
            let target = self
                .cw
                .class_ref(&crate::jvm::names::instanceof_internal_name(value));
            code.checkcast(target);
        }
        value
    }

    /// Emit `expression` and materialize it at the consumer's declared type.
    pub(super) fn emit_value_as(
        &mut self,
        expression: crate::ir::ExprId,
        expected: Ty,
        code: &mut CodeBuilder,
    ) {
        // A checked implicit coercion can name the call-site substitution even though the value
        // still occupies the declaration's erased bound. Let this consumer choose whether that
        // bound needs narrowing; eagerly emitting the coercion can cast `Comparable` to `String`
        // and then back to `Comparable` for a function that returns the bound itself.
        let source = if matches!(
            self.erased_reference_result(expression),
            Some(ErasedResult::Coerced { call, slot })
                if !jvm_is_erased_top(ir_ty_to_jvm(&slot))
                    || self.retains_declaration_result_slot(expression, call, slot)
        ) {
            self.emit_consumed_operand(expression, code)
        } else {
            self.emit_value(expression, code);
            self.value_ty(expression)
        };
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

    /// Defer only the checked declaration-result conversion while that call still owns its
    /// recorded reference slot. Inline expansion and suspension-result materialization own their
    /// replacement values; ordinary smart casts are separate checked conversions.
    fn retains_declaration_result_slot(
        &self,
        expression: crate::ir::ExprId,
        producer: crate::ir::ExprId,
        slot: Ty,
    ) -> bool {
        if !self.ir.declaration_result_coercions.contains(&expression)
            || self.ir.physical_types.get(&producer) != Some(&slot)
            || self.transformed_result(producer).is_some()
        {
            return false;
        }
        let Some(call) =
            crate::jvm::call_result_boundaries::declaration_result_call(self.ir, producer)
        else {
            return false;
        };
        !matches!(
            self.ir.expr(call),
            IrExpr::Call { callee, .. }
                if matches!(&**callee, crate::ir::Callee::Static { inline, .. } if inline.can_inline())
        ) && self.ir.compiled_inline_call(call).is_none()
            && self.transformed_result(call).is_none()
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
                // Keep the declaration-side erasure until the consumer chooses its physical type.
                // This is not limited to `Object`: `T : Comparable<T>` arrives as `Comparable`.
                // A String-only consumer casts that slot once to `String`; a consumer of the bound
                // itself uses it unchanged instead of casting to `String` and back to `Comparable`.
                // A real call records its descriptor result in `physical_types`; only its erased
                // top is this generic-result contract. A call recorded as inline can instead
                // retain its declaration's non-top erased bound while the JVM splicer replaces
                // it. Once inlining has removed the producer, `value_ty` reads that same active
                // body-local slot.
                let inline_producer = matches!(
                    self.ir.expr(*arg),
                    IrExpr::Call {
                        callee: call_callee,
                        ..
                    } if matches!(&**call_callee, crate::ir::Callee::Static { inline, .. } if inline.can_inline()));
                // Without a recorded result the producer's own value type is the slot. Only an
                // erased top is the generic-result contract; a narrower slot is a smart cast
                // (`p.type` proven `Ref.Cls`), which this coercion itself checks.
                let slot = self
                    .ir
                    .physical_types
                    .get(arg)
                    .copied()
                    .unwrap_or_else(|| self.value_ty(*arg));
                if !jvm_is_erased_top(ir_ty_to_jvm(&slot)) && !inline_producer {
                    return None;
                }
                narrows(slot, *type_operand).then_some(ErasedResult::Coerced { call: *arg, slot })
            }
            crate::ir::IrExpr::InvokeFunction { ret, .. } => {
                (!matches!(*ret, Ty::Unit | Ty::Nothing)
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

    /// Emit an erased scalar result consumed by a reference `slot` as the box its callee returned,
    /// answering the erased slot it occupies. kotlinc coerces the call's own `Object` value
    /// straight to the slot (`StackValue.coerce(Object, Object)` writes nothing), so the
    /// substituted primitive is never materialized. `None`, with nothing emitted, for any other
    /// operand or a scalar slot.
    pub(super) fn emit_erased_scalar_box(
        &mut self,
        expression: crate::ir::ExprId,
        slot: Ty,
        code: &mut CodeBuilder,
    ) -> Option<Ty> {
        if !ir_ty_to_jvm(&slot).is_reference() {
            return None;
        }
        let (call, erased) = self.erased_scalar_result(expression)?;
        self.emit_value(call, code);
        Some(erased)
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
            self.box_scalar_as_reference(semantic, source_jvm, physical, code);
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

    /// Materialize a scalar's wrapper at the consumer's reference type. The declared physical
    /// slot can retain a generic bound even when the checked argument is a concrete scalar.
    pub(super) fn box_scalar_as_reference(
        &mut self,
        semantic: Ty,
        physical: Ty,
        target: Ty,
        code: &mut CodeBuilder,
    ) {
        let scalar = semantic_scalar_adapter(semantic, physical);
        box_prim_free(self.cw, code, scalar);
        // kotlinc coerces the materialized wrapper even when this widens it to Number or
        // Comparable. Object and the same wrapper need no cast; unsigned carriers have their
        // own value-class adaptation instead of ordinary reference coercion.
        if let Some(wrapper) = scalar.jvm_boxed_ref() {
            self.coerce_reference_on_stack(wrapper, target, code);
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
        // Only a value coming out of erased `Object` is narrowed to a carried type here.
        if self.is_carried_value(source) || self.is_carried_value(target) {
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

    /// A value class or unsigned type (or an array of one) is carried as its underlying value,
    /// which the erased type does not name. Its representation belongs to the value-class adapter.
    pub(super) fn is_carried_value(&self, mut ty: Ty) -> bool {
        loop {
            ty = ty.non_null();
            if ty.is_unsigned() || self.is_value_class_ty(&ty) {
                return true;
            }
            match ty.array_elem() {
                Some(element) => ty = element,
                None => return false,
            }
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
            .and_then(|parameters| {
                super::capture_order::physical_for(
                    self.ir,
                    self.constructed_class(construction),
                    parameters.to_vec(),
                )
                .get(parameter_index)
                .copied()
            });
        self.adapt_physical_operand(source, semantic, destination_semantic, physical, code);
    }

    pub(super) fn constructor_physical_params(
        &self,
        construction: crate::ir::ExprId,
        descriptor: &str,
    ) -> Vec<Ty> {
        let owner = self.constructed_class(construction);
        let checked = self
            .ir
            .construction_declared_params
            .get(&construction)
            .map(|params| super::capture_order::physical_for(self.ir, owner, params.to_vec()));
        crate::jvm::physical_type::constructor_operand_tys(descriptor, checked.as_deref())
            .expect("constructor descriptor must be valid")
    }

    /// The class a construction builds; its checked parameters lead with that class's captures.
    fn constructed_class(&self, construction: crate::ir::ExprId) -> crate::types::TypeName {
        let crate::ir::IrExpr::New { internal, .. } = &self.ir.exprs[construction as usize] else {
            panic!("malformed IR: constructor operands of a non-construction expression");
        };
        *internal
    }
}
