//! Materialize checked semantic values at JVM consumption boundaries.
//!
//! Common IR has already selected the callable, argument mapping, and semantic types. This module
//! owns only the physical JVM transition applied immediately after an operand is emitted: scalar
//! boxing/unboxing, value-class carriers, numeric conversion, and reference stack coercion.

use super::*;

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
        self.coerce_reference_on_stack(self.value_ty(expression), expected, code);
    }

    /// Whether `ty` names a `@JvmInline value class`, whose values use a backend-owned carrier.
    pub(super) fn is_value_class_ty(&self, ty: &Ty) -> bool {
        ty.non_null().obj_internal().is_some_and(|fq_name| {
            self.ir
                .classes
                .iter()
                .any(|class| class.is_value && class.fq_name == fq_name)
                || self.ir.has_external_value_class_name(fq_name)
        })
    }

    /// Narrow an erased reference on top of the stack to its consumption type.
    pub(super) fn narrow_on_stack(&mut self, source: Ty, expected: Ty, code: &mut CodeBuilder) {
        let source = ir_ty_to_jvm(&source);
        if !jvm_is_erased_top(source) {
            return;
        }
        let expected = ir_ty_to_jvm(&expected);
        if !expected.is_reference() || type_descriptor(source) == type_descriptor(expected) {
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
        // A checked value-class value can reach a physical carrier slot as its BOX object (for
        // example, an element read from `Collection<Item>` inside an inlined `all` lambda). Primitive
        // wrapper unboxing is not applicable: `Item` is not `Integer`, even when its carrier is `int`.
        let boxed_value_class = semantic.non_null().obj_internal().filter(|classifier| {
            source_jvm.non_null().obj_internal() == Some(*classifier)
                && self.is_value_class_ty(&semantic)
        });
        let value_class_carrier = boxed_value_class.and_then(|classifier| {
            self.ir
                .value_class_underlying_name(classifier)
                .map(|underlying| jvm_declared_ty(&underlying))
        });
        if let (Some(classifier), Some(carrier)) = (boxed_value_class, value_class_carrier) {
            // A reference supertype/generic descriptor consumes the BOX itself. Unbox only when the
            // selected descriptor consumes this value class's actual carrier.
            let destination_is_concrete_value_class =
                destination_semantic.is_none_or(|destination| {
                    destination.non_null().obj_internal() == Some(classifier)
                });
            if destination_is_concrete_value_class
                && type_descriptor(carrier) == type_descriptor(physical)
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
        match self.reference_coercion(source, target) {
            ReferenceCoercion::Unchanged => {}
            ReferenceCoercion::Cast(internal) => {
                let class = self.cw.class_ref(&internal);
                code.checkcast(class);
            }
            ReferenceCoercion::Carried => self.narrow_on_stack(source, target, code),
        }
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
}
