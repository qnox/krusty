//! Emitting one already-selected realization of a property READ.
//!
//! WHICH realization a property read takes is decided elsewhere, from the owner's own class file or
//! from the convention kotlinc follows for a class this compilation is still emitting. What is left
//! is physical: push the receiver the realization wants, load the field or call the accessor, and
//! bridge the physical result to the property's Kotlin type. Keeping it apart is what lets the
//! selection be read without the emission underneath it.

use super::*;

impl Emitter<'_> {
    /// Emit one already-chosen realization of a property read: push the receiver (or drop it, when the
    /// realization takes none), perform the field load or accessor call, and bridge the physical result to
    /// the property read's Kotlin type.
    pub(super) fn emit_realized_property_read(
        &mut self,
        operation: crate::ir::ExprId,
        receiver: Option<crate::ir::ExprId>,
        access: crate::jvm::inline::PropertyAccess,
        ty: &Ty,
        code: &mut CodeBuilder,
    ) {
        use crate::jvm::inline::PropertyAccess;
        let exact_field = matches!(&access, PropertyAccess::Field { .. });
        // Kotlin treats the expression to the left of a static `@JvmField` READ as a qualifier and
        // does not evaluate it.  A write is observably different and still evaluates an explicit
        // receiver before `putstatic`; that rule remains in `emit_property_write`.
        let receiver_is_static_field_qualifier = matches!(
            &access,
            PropertyAccess::Field {
                is_static: true,
                ..
            }
        );
        let access_owner = match &access {
            PropertyAccess::Field { owner, .. }
            | PropertyAccess::Accessor { owner, .. }
            | PropertyAccess::AccessBridge { owner, .. } => owner.clone(),
        };
        let takes_receiver = accessor_takes_receiver(&access);
        let receiver_ty = accessor_receiver_ty(&access, &access_owner);
        if let Some(receiver) = receiver.filter(|_| !receiver_is_static_field_qualifier) {
            self.emit_property_receiver(
                receiver,
                &access_owner,
                takes_receiver,
                &receiver_ty,
                code,
            );
        } else if takes_receiver {
            self.run.set_emit_error(format!(
                "receiver-less property realization requires an instance receiver: {access_owner}"
            ));
            return;
        }
        let physical = match access {
            PropertyAccess::Field {
                owner,
                name,
                descriptor,
                is_static,
            } => {
                let jt = ty_from_field_descriptor(&descriptor);
                let lateinit = self.is_lateinit_field(&owner, &name);
                let fref = self.cw.fieldref(&owner, &name, &descriptor);
                if is_static {
                    code.getstatic(fref, slot_words(jt) as i32);
                } else {
                    code.getfield(fref, slot_words(jt) as i32);
                }
                // A `lateinit var` read throws while the field is still null, wherever it is read from.
                if lateinit {
                    code.dup();
                    let lbl = code.new_label();
                    code.ifnonnull(lbl);
                    code.push_string(&name, self.cw);
                    let m = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "throwUninitializedPropertyAccessException",
                        "(Ljava/lang/String;)V",
                    );
                    code.invokestatic(m, 1, 0);
                    let st = self.verif_stack(jt);
                    self.frame(lbl, st, code);
                    self.bind(lbl, code);
                }
                jt
            }
            PropertyAccess::Accessor {
                owner,
                name,
                descriptor,
                is_static,
                is_interface,
            } => {
                // A `void` accessor (a `Unit` property) leaves NOTHING on the stack — `descriptor_ret_words`
                // is the authority on that, since `ty_from_descriptor_ret` maps `V` to a 1-word `Unit` for
                // type flow. Nothing is left, so there is nothing to bridge.
                let words = descriptor_ret_words(&descriptor);
                let m = if is_interface {
                    self.cw.interface_methodref(&owner, &name, &descriptor)
                } else {
                    self.cw.methodref(&owner, &name, &descriptor)
                };
                // A read through an ACCESSOR is a dispatch, so the read's own line returns here,
                // after the receiver chain has marked its. A read realized as a FIELD is not one,
                // and deliberately marks nothing — the receiver's line stays in effect through the
                // `getfield`, exactly as kotlinc records it.
                self.mark_dispatch_line(operation, code);
                if is_static {
                    code.invokestatic(m, 0, words);
                } else if is_interface {
                    code.invokeinterface(m, 0, words);
                } else {
                    code.invokevirtual(m, 0, words);
                }
                if words == 0 {
                    return;
                }
                ty_from_descriptor_ret(&descriptor)
            }
            PropertyAccess::AccessBridge {
                owner,
                name,
                descriptor,
            } => {
                // The receiver is already on the stack as the bridge's sole argument.
                let words = descriptor_ret_words(&descriptor);
                let m = self.cw.methodref(&owner, &name, &descriptor);
                self.mark_dispatch_line(operation, code);
                code.invokestatic(m, 1, words);
                if words == 0 {
                    return;
                }
                ty_from_descriptor_ret(&descriptor)
            }
        };
        // The realization's result is the PHYSICAL one — erased to `Object` for a type parameter, a bare
        // primitive for an `Int` property. The node's `ty` is the logical Kotlin type the read has at this
        // site (`Int?` in a safe-call chain). Bridge the two exactly as any other physical result is
        // bridged: box, unbox, or narrow.
        let logical = ir_ty_to_jvm(&stored_value_ty(*ty));
        let value_class = self.is_value_class_ty(ty);
        if !value_class
            && physical.is_jvm_scalar()
            && !logical.is_jvm_scalar()
            && logical.is_reference()
        {
            box_prim_free(self.cw, code, semantic_scalar_adapter(*ty, physical));
        } else if !value_class && !physical.is_jvm_scalar() && logical.is_jvm_scalar() {
            // `ty` is the substituted semantic result and `logical` its JVM carrier. Choosing the
            // adapter from `logical` alone turns `UInt` into `Integer`; retain the semantic type until
            // after the `Object` boundary has been bridged.
            unbox_prim_from(
                self.cw,
                code,
                physical,
                semantic_scalar_adapter(*ty, logical),
            );
        } else if exact_field
            && physical.is_reference()
            && logical.is_reference()
            && type_descriptor(physical) != type_descriptor(logical)
            && !value_class
        {
            // A generic Java field's descriptor erases to its formal bound (`CharSequence` for
            // `T : CharSequence`), while this applied read may be `String`. Preserve the selected
            // field descriptor for `getfield`, then narrow its result to the logical binding.
            let internal = crate::jvm::names::instanceof_internal_name(logical);
            if internal != "java/lang/Object" {
                let class = self.cw.class_ref(&internal);
                code.checkcast(class);
            }
        } else if !value_class {
            // A value class has no runtime type of its own — its values ARE the erased underlying — so
            // narrowing to one would `checkcast` to a class the value is not an instance of.
            self.narrow_on_stack(physical, *ty, code);
        }
    }
}
