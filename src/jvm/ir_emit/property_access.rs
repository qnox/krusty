//! Emitting one already-selected realization of a property READ.
//!
//! WHICH realization a property read takes is decided elsewhere, from the owner's own class file or
//! from the convention kotlinc follows for a class this compilation is still emitting. What is left
//! is physical: push the receiver the realization wants, load the field or call the accessor, and
//! bridge the physical result to the property's Kotlin type. Keeping it apart is what lets the
//! selection be read without the emission underneath it.

use super::*;

/// Exact field selected for a property declared in this compilation.
///
/// A capture spliced ahead of a source property can share its IR spelling (`x` beside the capture
/// the JVM names `$x`). The property records its own backing-field index; a declaration with no
/// backing field is reached through its accessor. External fallback access has no local declaration
/// identity and retains the classfile-name lookup used at that boundary.
pub(super) fn declared_property_field<'a>(
    class: &'a crate::ir::IrClass,
    declared: Option<&crate::ir::IrProperty>,
    name: &str,
) -> Option<&'a crate::ir::IrField> {
    match declared {
        Some(property) => property
            .backing_field
            .and_then(|index| class.fields.get(index as usize)),
        None => class.fields.iter().find(|field| field.name == name),
    }
}

impl Emitter<'_> {
    /// Select the property-read realization available from declarations emitted by this compilation.
    /// `None` deliberately means the external bytecode-provider path must decide.
    fn selected_local_property_read_access(
        &self,
        owner: TypeName,
        name: &str,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        self.declared_property_read_access(owner, name, None, false)
    }

    /// Is `owner.name` a `lateinit` backing field of a class THIS compilation is emitting? Only such a
    /// field carries the inline uninitialized guard, so the read emission and [`Self::emits_control_flow`]
    /// must answer this one question the same way — a disagreement is a `VerifyError` at link time.
    fn is_lateinit_field(&self, owner: TypeName, name: &str) -> bool {
        self.ir
            .classes
            .iter()
            .find(|class| class.fq_name == owner)
            .and_then(|class| class.fields.iter().find(|field| field.name == name))
            .is_some_and(|field| field.is_lateinit())
    }

    /// Whether this property read emits the uninitialized guard inline. A direct field load does.
    /// So does a synthetic `access$get<X>$p` bridge: that bridge is a raw field load, not a getter
    /// body, so the guard is not hiding inside an accessor. A real getter still owns its own guard.
    pub(super) fn lateinit_read_guards_inline(&self, owner: TypeName, name: &str) -> bool {
        use crate::jvm::inline::PropertyAccess;
        let Some(access) = self.selected_local_property_read_access(owner, name) else {
            return false;
        };
        match access {
            PropertyAccess::Field { owner, name, .. } => self.is_lateinit_field(owner, &name),
            PropertyAccess::AccessBridge {
                inline_uninitialized_guard,
                ..
            } => inline_uninitialized_guard.is_some(),
            PropertyAccess::Accessor { .. } => false,
        }
    }

    /// Emit one already-chosen realization of a property read: push the receiver (or drop it, when the
    /// realization takes none), perform the field load or accessor call, and bridge the physical result to
    /// the property read's Kotlin type.
    pub(super) fn emit_realized_property_read(
        &mut self,
        operation: &PropertyOperation<'_>,
        access: crate::jvm::inline::PropertyAccess,
        code: &mut CodeBuilder,
    ) {
        use crate::jvm::inline::PropertyAccess;
        let access =
            access_bridges::protected_property_access(self.run, operation.expression, access);
        let (access, retarget_result_narrow) =
            self.planned_overridden_read_realization(operation, access);
        let Some(access) = self.checked_dispatched_accessor(operation.expression, access) else {
            return;
        };
        let receiver = operation.receiver;
        let ty = operation.ty;
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
            | PropertyAccess::AccessBridge { owner, .. } => *owner,
        };
        let takes_receiver = accessor_takes_receiver(&access);
        let receiver_ty = accessor_receiver_ty(&access, access_owner);
        if let Some(receiver) = receiver.filter(|_| !receiver_is_static_field_qualifier) {
            self.emit_property_receiver(receiver, access_owner, takes_receiver, &receiver_ty, code);
        } else if takes_receiver {
            self.run.set_emit_error(format!(
                "receiver-less property realization requires an instance receiver: {}",
                access_owner.render()
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
                let slot = crate::jvm::physical_type::field_slot(&descriptor);
                let lateinit = self.is_lateinit_field(owner, &name);
                let owner = owner.render();
                let fref = self.cw.fieldref(&owner, &name, &descriptor);
                if is_static {
                    code.getstatic(fref, slot.words());
                } else {
                    code.getfield(fref, slot.words());
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
                    self.bind(lbl, code);
                }
                (slot.ty, slot.reference)
            }
            PropertyAccess::Accessor {
                owner,
                name,
                descriptor,
                is_static,
                is_interface,
                static_receiver: _,
            } => {
                let owner = owner.render();
                // A `void` accessor (a `Unit` property) leaves NOTHING on the stack. The descriptor's
                // own slot width is the authority: `V` is zero words, and a class descriptor is one
                // reference word even when its classifier is an unsigned scalar.
                let slot = crate::jvm::physical_type::method_return_slot(&descriptor);
                let words = slot.words();
                let m = if is_interface {
                    self.cw.interface_methodref(&owner, &name, &descriptor)
                } else {
                    self.cw.methodref(&owner, &name, &descriptor)
                };
                // A read through an ACCESSOR is a dispatch, so the read's own line returns here,
                // after the receiver chain has marked its. A read realized as a FIELD is not one,
                // and deliberately marks nothing — the receiver's line stays in effect through the
                // `getfield`, exactly as kotlinc records it.
                self.mark_dispatch_line(operation.expression, code);
                if is_static {
                    let arguments = crate::jvm::names::parse_method_descriptor(&descriptor)
                        .expect("a planned property accessor has a valid JVM descriptor")
                        .0
                        .iter()
                        .map(|parameter| slot_words(ty_from_field_descriptor(parameter)) as i32)
                        .sum();
                    code.invokestatic(m, arguments, words);
                } else if is_interface {
                    code.invokeinterface(m, 0, words);
                } else {
                    code.invokevirtual(m, 0, words);
                }
                if let Some(narrow) = retarget_result_narrow {
                    let internal = crate::jvm::names::instanceof_internal_name(narrow);
                    let class = self.cw.class_ref(&internal);
                    code.checkcast(class);
                }
                if words == 0 {
                    return;
                }
                (slot.ty, slot.reference)
            }
            PropertyAccess::AccessBridge {
                owner,
                name,
                descriptor,
                inline_uninitialized_guard,
                ..
            } => {
                // The bridge's arguments are already on the stack: the receiver when it takes one,
                // and nothing when the field is a named object's static.
                let owner = owner.render();
                let slot = crate::jvm::physical_type::method_return_slot(&descriptor);
                let words = slot.words();
                let (parameters, _) = crate::jvm::names::parse_method_descriptor(&descriptor)
                    .expect("a planned property access bridge has a valid JVM descriptor");
                let arguments = parameters
                    .iter()
                    .map(|parameter| crate::jvm::physical_type::field_slot(parameter).words())
                    .sum();
                let m = self.cw.methodref(&owner, &name, &descriptor);
                self.mark_dispatch_line(operation.expression, code);
                code.invokestatic(m, arguments, words);
                if words == 0 {
                    return;
                }
                // `access$get<X>$p` is a raw field load. A `lateinit` getter is not synthesized for
                // a private property, so the uninitialized guard has to sit at this read, the same
                // place a direct field load puts it.
                if let Some(property) = inline_uninitialized_guard {
                    code.dup();
                    let initialized = code.new_label();
                    code.ifnonnull(initialized);
                    code.push_string(&property, self.cw);
                    let throw_uninitialized = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "throwUninitializedPropertyAccessException",
                        "(Ljava/lang/String;)V",
                    );
                    code.invokestatic(throw_uninitialized, 1, 0);
                    self.bind(initialized, code);
                }
                (slot.ty, slot.reference)
            }
        };
        // The realization's result is the PHYSICAL one — erased to `Object` for a type parameter, a bare
        // primitive for an `Int` property. The node's `ty` is the logical Kotlin type the read has at this
        // site (`Int?` in a safe-call chain). `reference_slot` is the descriptor category, so a class
        // descriptor is not boxed again just because its classifier is also a semantic scalar.
        let (physical, reference_slot) = physical;
        // An annotation member declared `KClass` is returned as `java.lang.Class`. The Kotlin
        // value is the rebuilt `KClass` (`Int::class` equals `Integer::class`).
        let physical = if reference_slot {
            crate::jvm::annotation_kclass::adapt_read_kclass(self.cw, code, *ty, physical)
        } else {
            physical
        };
        let logical = ir_ty_to_jvm(&stored_value_ty(*ty));
        let value_class = self.is_value_class_ty(ty);
        if !value_class && !reference_slot && logical.is_reference() && !logical.is_jvm_scalar() {
            box_prim_free(self.cw, code, semantic_scalar_adapter(*ty, physical));
        } else if !value_class && reference_slot && logical.is_jvm_scalar() {
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
            && !crate::jvm::names::same_type_descriptor(physical, logical)
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

    /// Apply the exact inherited-call plan selected before emission. A field read or access bridge
    /// is not an ordinary virtual call and deliberately ignores the plan.
    fn planned_overridden_read_realization(
        &self,
        operation: &PropertyOperation<'_>,
        access: crate::jvm::inline::PropertyAccess,
    ) -> (crate::jvm::inline::PropertyAccess, Option<Ty>) {
        use crate::jvm::inline::PropertyAccess;
        let Some(realization) = self
            .ir
            .jvm_overridden_call_realizations
            .get(&operation.expression)
        else {
            return (access, None);
        };
        match access {
            PropertyAccess::Accessor {
                owner,
                descriptor,
                is_static: false,
                is_interface,
                static_receiver: None,
                ..
            } => {
                let declared_ret = ty_from_descriptor_ret(&descriptor);
                let widened_ret = ty_from_descriptor_ret(&realization.descriptor);
                // A boxed getter (`getSize()Integer`) realized through a scalar slot
                // (`Collection.size()I`) reads the scalar itself: there is nothing to narrow.
                let narrow = (declared_ret != widened_ret
                    && declared_ret.is_reference()
                    && widened_ret.is_reference())
                .then_some(declared_ret);
                (
                    PropertyAccess::Accessor {
                        owner,
                        name: realization.physical_name.clone(),
                        descriptor: realization.descriptor.clone(),
                        is_static: false,
                        is_interface,
                        static_receiver: None,
                    },
                    narrow,
                )
            }
            access => (access, None),
        }
    }

    /// An instance accessor call names the class its dispatch receiver statically has, as any
    /// virtual call does (`Leaf.getBase`, not `Base.getBase`).
    pub(super) fn dispatched_accessor(
        &self,
        operation: crate::ir::ExprId,
        access: crate::jvm::inline::PropertyAccess,
    ) -> Result<crate::jvm::inline::PropertyAccess, crate::jvm::member_dispatch::MissingClassifier>
    {
        use crate::jvm::inline::PropertyAccess;
        match access {
            PropertyAccess::Accessor {
                owner,
                name,
                descriptor,
                is_static: false,
                is_interface,
                static_receiver,
            } => {
                let (owner, is_interface) = crate::jvm::member_dispatch::call_owner(
                    self.dispatch_classifiers.as_ref(),
                    owner,
                    is_interface,
                    self.ir.dispatch_classes.get(&operation).copied(),
                )?;
                Ok(PropertyAccess::Accessor {
                    owner,
                    name,
                    descriptor,
                    is_static: false,
                    is_interface,
                    static_receiver,
                })
            }
            access => Ok(access),
        }
    }
}

/// Whether a realized property accessor consumes the receiver as an operand. An instance accessor
/// always does. A static one does only when its provider/declaration recorded the physical carrier
/// parameter explicitly; an ordinary `@JvmStatic` accessor consumes no receiver.
pub(super) fn accessor_takes_receiver(access: &crate::jvm::inline::PropertyAccess) -> bool {
    use crate::jvm::inline::PropertyAccess;
    match access {
        PropertyAccess::Field { is_static, .. } => !is_static,
        PropertyAccess::Accessor {
            is_static,
            static_receiver,
            ..
        } => !is_static || static_receiver.is_some(),
        // An instance bridge takes the receiver; a named object's static field bridge does not.
        PropertyAccess::AccessBridge { takes_receiver, .. } => *takes_receiver,
    }
}

/// The type the receiver must hold ON THE STACK for `access`, given the property's `owner`.
///
/// Normally the owner itself. A static value-class accessor records its erased carrier explicitly;
/// narrowing that operand to the semantic owner would emit a `checkcast` no unboxed carrier can pass.
pub(super) fn accessor_receiver_ty(
    access: &crate::jvm::inline::PropertyAccess,
    owner: TypeName,
) -> Ty {
    use crate::jvm::inline::PropertyAccess;
    if let PropertyAccess::Accessor {
        static_receiver: Some(receiver),
        ..
    } = access
    {
        return *receiver;
    }
    Ty::obj_name(owner)
}

/// Physical type a property write stores. A class descriptor is a reference slot even when its
/// classifier is a semantic scalar; the checked property type supplies that reference.
pub(super) fn property_store_slot(access: &crate::jvm::inline::PropertyAccess, checked: Ty) -> Ty {
    use crate::jvm::inline::PropertyAccess;
    let descriptor = match access {
        PropertyAccess::Field { descriptor, .. } => Some(descriptor.as_str()),
        PropertyAccess::Accessor {
            descriptor,
            static_receiver,
            ..
        } => crate::jvm::names::parse_method_descriptor(descriptor).and_then(|(parameters, _)| {
            if static_receiver.is_some() {
                parameters.last().copied()
            } else {
                parameters.first().copied()
            }
        }),
        PropertyAccess::AccessBridge { descriptor, .. } => {
            crate::jvm::names::parse_method_descriptor(descriptor)
                .and_then(|(params, _)| params.last().copied())
        }
    };
    descriptor
        .map(|descriptor| crate::jvm::physical_type::operand_slot_ty(descriptor, Some(checked)))
        .unwrap_or_else(|| ir_ty_to_jvm(&checked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_accessor_receiver_is_an_explicit_realization_fact() {
        let owner = crate::types::type_name("review/OwnedValue");
        let same_spelling = crate::jvm::inline::PropertyAccess::Accessor {
            owner,
            name: "owned-impl".to_string(),
            descriptor: "(I)I".to_string(),
            is_static: true,
            is_interface: false,
            static_receiver: None,
        };
        assert!(!accessor_takes_receiver(&same_spelling));
        assert_eq!(
            accessor_receiver_ty(&same_spelling, owner),
            Ty::obj_name(owner)
        );

        let recorded = crate::jvm::inline::PropertyAccess::Accessor {
            owner,
            name: "owned-impl".to_string(),
            descriptor: "(I)I".to_string(),
            is_static: true,
            is_interface: false,
            static_receiver: Some(Ty::Int),
        };
        assert!(accessor_takes_receiver(&recorded));
        assert_eq!(accessor_receiver_ty(&recorded, owner), Ty::Int);
    }
}
