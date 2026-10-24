//! Selecting physical access for a property declared by the module being emitted.
//!
//! Local declarations have no class file to query. Their recorded property, field, accessor, and
//! bridge identities are therefore the complete source of the JVM access shape.

use super::*;

impl Emitter<'_> {
    /// Select the JVM access for an active-source property by the exact declaration coordinate
    /// retained from checked FIR. Companion storage may already have been moved to the outer class;
    /// consume that backend table directly instead of recovering the property from its spelling.
    pub(super) fn local_property_read_access(
        &self,
        target: crate::fir::PropertyId,
        selected_accessor: Option<&str>,
        selected_interface: bool,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            name,
            property,
            ..
        } = self.ir.local_property_layouts.get(&target)?
        else {
            return None;
        };
        debug_assert_eq!(self.ir.classes[*class as usize].fq_name, *owner);
        self.declared_property_read_access(
            *owner,
            name,
            Some(*property as usize),
            selected_accessor,
            selected_interface,
        )
    }

    /// Whether a property of `owner` may be reached as its raw backing FIELD from the class currently
    /// being emitted. Only inside the declaring class (the field is private everywhere else) — and only
    /// for a FINAL property. An `open`/`override` property is redeclared by subclasses, which replace its
    /// ACCESSOR, not the base's own private storage: a `getfield` from a base method would read the
    /// base's field and silently bypass the override. kotlinc emits `invokevirtual get<Name>()` inside
    /// the class for exactly that reason, so the accessor is the only correct realization here.
    ///
    /// Two exemptions, both because the accessor an `open` property would be reached through does not
    /// exist:
    ///
    /// * a PRIVATE property has no synthesized accessor at all (kotlinc reads it directly in-class).
    ///   `private open` is not valid Kotlin — kotlinc reports "'open' is incompatible with 'private'"
    ///   — so this only decides what an input krusty accepts but kotlinc rejects compiles to, and the
    ///   raw field is the realization that at least links.
    /// * a `val` has no SETTER, so a `writable` access to one can only be the deferred initialization
    ///   Kotlin permits in a constructor/`init` block, which kotlinc also emits as a `putfield`.
    ///
    /// A `@JvmField` property is reachable this way from ANY class: it has no accessor to call, and
    /// its field carries the declaration's own visibility rather than Kotlin's default `private`.
    pub(super) fn direct_field_access(
        &self,
        class: &crate::ir::IrClass,
        declared: Option<&crate::ir::IrProperty>,
        writable: bool,
    ) -> bool {
        if declared.is_some_and(|p| is_jvm_field(class, &p.name)) {
            return true;
        }
        // A body a caller may splice into another class cannot name this class's private field: a
        // non-private inline function reaches a non-private property through its getter or setter,
        // as kotlinc does in a public inline scope. A private property keeps its exported field
        // accessor. A writable `val` remains a direct deferred-initialization write because it has
        // no setter. A private default setter has no method either: select its field so emission can
        // replace that field access with the planned export bridge.
        if self.export_private_calls
            && declared.is_some_and(|p| {
                !p.is_private && (!writable || (p.is_var && !p.has_private_default_setter()))
            })
        {
            return false;
        }
        // An explicit backing field is a different type from the property. The checker already
        // chose a field read, and lowered it as one, when the receiver's static type is exactly
        // this class. A property read that remains is the getter: a subclass value, a nested
        // class, and every other receiver. Loading the private field here would skip that choice
        // and hand back the carrier (`Integer.valueOf`) instead of the getter's public value.
        if !writable && declared.is_some_and(|property| property.storage_ty.is_some()) {
            return false;
        }
        class.fq_name_matches(&self.owner)
            && !declared.is_some_and(|p| {
                // kotlinc keeps a non-private `lateinit` property's accessors in-class too: the
                // getter is where its uninitialized check lives.
                let lateinit = p
                    .backing_field
                    .and_then(|field| class.fields.get(field as usize))
                    .is_some_and(|field| field.is_lateinit());
                (p.is_open || lateinit)
                    && !p.is_private
                    && (!writable || (p.is_var && !p.has_private_default_setter()))
            })
    }

    /// The write through a setter declared in this compilation. A value class's computed setter is
    /// realized as a static over the carrier (`setX-impl(carrier, value)`), so the write passes the
    /// carrier as that static's receiver, exactly as a read through its getter does.
    pub(super) fn declared_setter_access(
        &self,
        class: &crate::ir::IrClass,
        owner: TypeName,
        setter: u32,
    ) -> crate::jvm::inline::PropertyAccess {
        let f = &self.ir.functions[setter as usize];
        let (descriptor, static_receiver) = if f.is_static {
            (
                ir_method_desc(&f.params, &f.ret),
                (f.dispatch_receiver == Some(owner))
                    .then(|| f.params.first().map(jvm_declared_ty))
                    .flatten(),
            )
        } else {
            (
                method_descriptor(&[jvm_declared_ty(&f.params[0])], Ty::Unit),
                None,
            )
        };
        crate::jvm::inline::PropertyAccess::Accessor {
            owner,
            name: f.name.clone(),
            descriptor,
            is_static: f.is_static,
            is_interface: is_jvm_interface(class),
            static_receiver,
        }
    }

    /// How to read property `name` of a class THIS compilation declares — there is no class file to ask,
    /// the IR is the declaration. Inside the declaring class the private backing field is loaded directly,
    /// which is what kotlinc emits there; from outside, the read goes through the accessor. `None` when
    /// `owner` is not a class of this file, or declares no such property.
    pub(super) fn declared_property_read_access(
        &self,
        owner: TypeName,
        name: &str,
        selected_property: Option<usize>,
        selected_accessor: Option<&str>,
        selected_interface: bool,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        use crate::jvm::inline::PropertyAccess;
        let class = self.ir.classes.iter().find(|c| c.fq_name == owner)?;
        let declared_index = selected_property.or_else(|| {
            class
                .properties
                .iter()
                .position(|property| property.name == name)
        });
        if let Some(index) = declared_index {
            if let Some(access) =
                self.hoisted_companion_property_access(class.fq_name, index as u32, false)
            {
                return Some(access);
            }
        }
        let interface = is_jvm_interface(class) || selected_interface;
        // A property that DECLARES an accessor (computed, delegated, or `field`-using) is always read
        // through it — the accessor is user code, and a direct field load would skip it. Only a plain
        // backing-field property may be read directly, and only from inside the declaring class.
        let declared = declared_index.map(|index| &class.properties[index]);
        // A scalar getter result over a reference-returning overridden getter is the wrapper;
        // the read unboxes it (see `jvm::override_results`).
        let boxed_getter = declared_index.is_some_and(|index| {
            self.override_results
                .boxes_member_property(owner, index as u32)
        });
        let direct_field = self.direct_field_access(class, declared, false);
        if let Some(getter) = declared.and_then(|p| p.getter) {
            let f = &self.ir.functions[getter as usize];
            // Another class reads a private getter through its bridge, kotlinc's `access$<getter>`.
            if self.reaches_through_bridge(class.fq_name, getter) {
                return Some(access_bridges::private_member_accessor_access(
                    self.ir, getter, owner,
                ));
            }
            return Some(PropertyAccess::Accessor {
                owner,
                name: if class.is_annotation {
                    name.to_string()
                } else {
                    f.name.clone()
                },
                descriptor: if class.is_annotation {
                    annotation_member_descriptor(&f.params, f.ret)
                } else {
                    ir_method_desc(
                        &f.params,
                        &self.override_results.physical_result(self.ir, getter),
                    )
                },
                is_static: f.is_static,
                is_interface: interface,
                static_receiver: (f.is_static && f.dispatch_receiver == Some(owner))
                    .then(|| f.params.first().map(jvm_declared_ty))
                    .flatten(),
            });
        }
        let field = property_access::declared_property_field(class, declared, name);
        // A declaration-specified JVM name wins; otherwise the checker's selected accessor identity
        // refines the naming convention. Backend value-class mangling lives in a different table and
        // therefore cannot overwrite an inherited generic declaration here.
        let accessor_name = if class.is_annotation {
            name.to_string()
        } else {
            declared
                .and_then(|p| p.getter_jvm_name.clone())
                .or_else(|| selected_accessor.map(str::to_string))
                .unwrap_or_else(|| crate::names::property_getter_name(name))
        };
        // The accessor's descriptor comes from the accessor ITSELF, not from the field: an accessor may
        // return something the field's declared type does not spell (an erased generic, a value class's
        // underlying), and a descriptor built from the wrong one is a `NoSuchMethodError` at run time.
        // A value-class-typed property's accessor is `@JvmName`-mangled (`getId-<hash>`), so match the
        // mangled spelling too — the alternative is falling through to a private backing field, which is
        // an `IllegalAccessError` from anywhere but the declaring class.
        let accessor = declared
            .is_none()
            .then(|| {
                class.methods.iter().copied().find(|&fid| {
                    let f = &self.ir.functions[fid as usize];
                    let named = f.name == accessor_name
                        || f.name
                            .strip_prefix(&accessor_name)
                            .is_some_and(|rest| rest.starts_with('-'));
                    let getter_shape = f.params.is_empty()
                        || (f.is_static && f.name.ends_with("-impl") && f.params.len() == 1);
                    named && getter_shape
                })
            })
            .flatten();
        if let Some(function) = accessor.filter(|_| !direct_field || field.is_none()) {
            let accessor = &self.ir.functions[function as usize];
            return Some(PropertyAccess::Accessor {
                owner,
                name: accessor.name.clone(),
                descriptor: ir_method_desc(
                    &accessor.params,
                    &self.override_results.physical_result(self.ir, function),
                ),
                is_static: accessor.is_static,
                is_interface: interface,
                static_receiver: (accessor.is_static && accessor.dispatch_receiver == Some(owner))
                    .then(|| accessor.params.first().map(jvm_declared_ty))
                    .flatten(),
            });
        }
        // A private property reached from outside its class goes through the synthetic bridge; there is no
        // accessor and the field itself is unreachable. A write may have marked a public property whose
        // default setter alone is private; its reads still use the public getter.
        if let Some(declared) = declared.filter(|p| {
            p.is_private
                && p.needs_access_bridge
                && self.static_owner != Some(StaticOwner::Class(class.fq_name))
        }) {
            let ty = declared
                .backing_field
                .and_then(|i| class.fields.get(i as usize))
                .map_or(declared.ty, |f| f.ty);
            let d = type_descriptor(jvm_declared_ty(&ty));
            return Some(static_accessors::member_property_access_bridge(
                self.ir, class, owner, declared, &d, true,
            ));
        }
        // A class of THIS compilation is answered from its declaration, always — never by falling through
        // to the naming-convention guess, which has no class file to ask and would mistake an interface
        // for a class (`invokevirtual` on an interface is an `IncompatibleClassChangeError`).
        let Some((field_index, field)) = field else {
            let ty = declared.map(|p| p.ty)?;
            return Some(PropertyAccess::Accessor {
                owner,
                name: accessor_name,
                descriptor: if class.is_annotation {
                    annotation_member_descriptor(&[], ty)
                } else if boxed_getter {
                    ir_method_desc(&[], &Ty::nullable(ty))
                } else {
                    ir_method_desc(&[], &stored_value_ty(ty))
                },
                is_static: false,
                is_interface: interface,
                static_receiver: None,
            });
        };
        // Outside the declaring class the backing field is private, so the read goes through the
        // accessor — the one synthesized for this declaration, which carries no IR method of its own.
        if !direct_field {
            let return_ty = declared
                .map(|property| {
                    if boxed_getter {
                        jvm_declared_ty(&Ty::nullable(property.ty))
                    } else {
                        declared_property_accessor_jvm(self.ir, property, field)
                    }
                })
                .unwrap_or_else(|| jvm_value_ty(&field.ty));
            let return_ty = if class.is_annotation {
                crate::jvm::annotation_kclass::annotation_member_jvm_type(return_ty)
            } else {
                return_ty
            };
            return Some(PropertyAccess::Accessor {
                owner,
                name: accessor_name,
                descriptor: method_descriptor(&[], return_ty),
                is_static: false,
                is_interface: interface,
                static_receiver: None,
            });
        }
        Some(PropertyAccess::Field {
            owner,
            name: instance_field_jvm_name(self.ir, self.run, class, field_index),
            descriptor: type_descriptor(jvm_value_ty(&field.ty)),
            // A static-storage object's backing fields are JVM statics (kotlinc's shape).
            is_static: static_storage(self.ir, class),
        })
    }

    pub(super) fn local_property_write_access(
        &self,
        target: crate::fir::PropertyId,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            name,
            property,
            ..
        } = self.ir.local_property_layouts.get(&target)?
        else {
            return None;
        };
        let class = self.ir.classes.get(*class as usize)?;
        debug_assert_eq!(class.fq_name, *owner);
        let index = *property as usize;
        let declared = class.properties.get(index)?;
        self.realize_declared_property_write_access(class, *owner, name, Some((index, declared)))
    }

    /// The write analogue of [`Self::declared_property_read_access`] for a node that carries no
    /// stable local [`crate::fir::PropertyId`]. Checked local operations use
    /// [`Self::local_property_write_access`] and never enter this source-spelling lookup.
    pub(super) fn declared_property_write_access(
        &self,
        owner: TypeName,
        name: &str,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let class = self.ir.classes.iter().find(|c| c.fq_name == owner)?;
        let declared = class
            .properties
            .iter()
            .position(|property| property.name == name)
            .map(|index| (index, &class.properties[index]));
        self.realize_declared_property_write_access(class, owner, name, declared)
    }

    fn realize_declared_property_write_access(
        &self,
        class: &crate::ir::IrClass,
        owner: TypeName,
        name: &str,
        declared: Option<(usize, &crate::ir::IrProperty)>,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        use crate::jvm::inline::PropertyAccess;
        if let Some((index, _)) = declared {
            if let Some(access) =
                self.hoisted_companion_property_access(class.fq_name, index as u32, true)
            {
                return Some(access);
            }
        }
        // The write analogue: a declared setter is user code and must not be bypassed.
        let declared = declared.map(|(_, property)| property);
        let direct_field = self.direct_field_access(class, declared, true);
        if let Some(declared) = declared.filter(|p| {
            p.needs_access_bridge && self.static_owner != Some(StaticOwner::Class(class.fq_name))
        }) {
            let ty = declared
                .backing_field
                .and_then(|i| class.fields.get(i as usize))
                .map_or(declared.ty, |f| f.ty);
            let d = type_descriptor(jvm_declared_ty(&ty));
            return Some(static_accessors::member_property_access_bridge(
                self.ir, class, owner, declared, &d, false,
            ));
        }
        if let Some(setter) = declared.and_then(|p| p.setter) {
            if self.reaches_through_bridge(class.fq_name, setter) {
                return Some(access_bridges::private_member_accessor_access(
                    self.ir, setter, owner,
                ));
            }
            return Some(self.declared_setter_access(class, owner, setter));
        }
        let field = property_access::declared_property_field(class, declared, name);
        let setter_name = declared
            .and_then(|p| p.setter_jvm_name.clone())
            .unwrap_or_else(|| crate::names::property_setter_name(name));
        let setter = declared
            .is_none()
            .then(|| {
                class.methods.iter().find_map(|&fid| {
                    let f = &self.ir.functions[fid as usize];
                    let named = f.name == setter_name
                        || f.name
                            .strip_prefix(&setter_name)
                            .is_some_and(|rest| rest.starts_with('-'));
                    (named && f.params.len() == 1).then_some(f)
                })
            })
            .flatten();
        // Outside the declaring class the backing field is private, so the write goes through the setter
        // — and a property with no backing field at all (a custom setter, a delegated one) is written
        // through it from anywhere.
        let accessor = |f: &crate::ir::IrFunction| PropertyAccess::Accessor {
            owner,
            name: f.name.clone(),
            descriptor: method_descriptor(&[jvm_declared_ty(&f.params[0])], Ty::Unit),
            is_static: false,
            is_interface: is_jvm_interface(class),
            static_receiver: None,
        };
        if let Some(setter) = setter.filter(|_| !direct_field || field.is_none()) {
            return Some(accessor(setter));
        }
        let (field_index, field) = field?;
        if !direct_field {
            return Some(PropertyAccess::Accessor {
                owner,
                name: setter_name,
                descriptor: method_descriptor(&[jvm_value_ty(&field.ty)], Ty::Unit),
                is_static: false,
                is_interface: is_jvm_interface(class),
                static_receiver: None,
            });
        }
        Some(PropertyAccess::Field {
            owner,
            name: instance_field_jvm_name(self.ir, self.run, class, field_index),
            descriptor: type_descriptor(jvm_value_ty(&field.ty)),
            // A static-storage object's backing fields are JVM statics (kotlinc's shape).
            is_static: static_storage(self.ir, class),
        })
    }

    /// A companion property whose storage was moved onto the outer class.
    ///
    /// The outer class, including its `<clinit>`, reads and writes the private static field.
    /// A private property has no accessor, so every other class uses `access$get…$cp` /
    /// `access$set…$cp`. A public property's accessor stays the companion getter or setter.
    pub(super) fn hoisted_companion_property_access(
        &self,
        companion: TypeName,
        property: u32,
        writable: bool,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        use crate::jvm::inline::PropertyAccess;
        let storage = hoisted_companion_storage(self.ir, companion, property, writable)?;
        let declaration = self
            .ir
            .classes
            .iter()
            .find(|class| class.fq_name == companion)?
            .properties
            .get(property as usize)?;
        // A source assignment retained in the companion initializer still calls a visible default
        // `lateinit` setter. The outer `<clinit>` owns storage initialization, but that must not
        // erase the source accessor boundary (`Companion.setText(value)`).
        if writable
            && self.render_initializer_boundaries
            && self.ir.statics[storage.index as usize].is_lateinit
            && matches!(
                declaration.setter_visibility,
                crate::types::Visibility::Public | crate::types::Visibility::Internal
            )
        {
            let setter = declaration.setter?;
            let function = &self.ir.functions[setter as usize];
            return Some(PropertyAccess::Accessor {
                owner: companion,
                name: declaration
                    .setter_jvm_name
                    .clone()
                    .unwrap_or_else(|| function.name.clone()),
                descriptor: method_descriptor(&[jvm_declared_ty(&function.params[0])], Ty::Unit),
                is_static: false,
                is_interface: false,
                static_receiver: None,
            });
        }
        // kotlinc keeps an accessor call in an inline function's body, which other classes
        // inline: the companion's own accessor, when that side has one.
        let in_inline_body = self
            .function
            .is_some_and(|function| self.ir.inline_fns.contains(&function));
        let emitted_by_owner = self.static_owner == Some(StaticOwner::Class(storage.owner))
            && !(in_inline_body && !storage.bridged);
        if emitted_by_owner || self.ir.is_jvm_field_static(storage.index) {
            return Some(PropertyAccess::Field {
                owner: storage.owner,
                name: self.ir.static_field_jvm_name(storage.index).to_string(),
                descriptor: storage.descriptor(self.ir),
                is_static: true,
            });
        }
        if !storage.bridged {
            return None;
        }
        let (name, descriptor) = storage.bridge_shape(self.ir, writable);
        Some(PropertyAccess::AccessBridge {
            owner: storage.owner,
            name,
            descriptor,
            takes_receiver: false,
            // The `$cp` bridge exposes raw hoisted storage. A normal private `lateinit` read owns
            // the guard at this use site; `::property.isInitialized` is a distinct IR operation
            // and never selects this property-access realization.
            inline_uninitialized_guard: (!writable
                && self.ir.statics[storage.index as usize].is_lateinit)
                .then(|| self.ir.statics[storage.index as usize].name.clone()),
        })
    }
}

/// The outer-class static field that holds a hoisted companion property, as one use reaches it.
pub(super) struct HoistedCompanionStorage {
    pub(super) index: u32,
    /// The outer class that declares the field.
    pub(super) owner: TypeName,
    /// A class other than `owner` reaches the field through `access$get…$cp` / `access$set…$cp`:
    /// the property (for a read) or its default setter (for a write) is private, so that side has
    /// no callable companion accessor.
    pub(super) bridged: bool,
}

impl HoistedCompanionStorage {
    fn descriptor(&self, ir: &IrFile) -> String {
        type_descriptor(jvm_declared_ty(&ir.statics[self.index as usize].ty))
    }

    /// The name and descriptor of the outer class's `$cp` bridge for a read, or a write.
    pub(super) fn bridge_shape(&self, ir: &IrFile, writable: bool) -> (String, String) {
        hoisted_bridge_shape(ir, self.index, writable)
    }
}

/// `access$get<X>$cp` / `access$set<X>$cp` of hoisted static `index`, with its descriptor.
pub(super) fn hoisted_bridge_shape(ir: &IrFile, index: u32, writable: bool) -> (String, String) {
    let storage = &ir.statics[index as usize];
    let descriptor = type_descriptor(jvm_declared_ty(&storage.ty));
    if writable {
        (
            format!("access${}$cp", property_setter_name(&storage.name)),
            format!("({descriptor})V"),
        )
    } else {
        (
            format!("access${}$cp", property_getter_name(&storage.name)),
            format!("(){descriptor}"),
        )
    }
}

/// The hoisted field a read (or write) of companion property `property` uses, when the
/// property's field moved to the outer class and the use is not a declared accessor's call: a
/// declared accessor is user code, which the outer `<clinit>` calls too (through `access$get…` /
/// `access$set…` when it is private). The accessor body and the declaration initializer store are
/// the field operations.
pub(super) fn hoisted_companion_storage(
    ir: &IrFile,
    companion: TypeName,
    property: u32,
    writable: bool,
) -> Option<HoistedCompanionStorage> {
    let index = ir.jvm_companion_property_static(companion, property)?;
    let owner = ir.statics.get(index as usize)?.owner?;
    let declared = ir
        .classes
        .iter()
        .find(|class| class.fq_name == companion)?
        .properties
        .get(property as usize)?;
    let calls_declared_accessor = if writable {
        declared.modifiers.declared_setter
    } else {
        declared.modifiers.declared_getter
    };
    if calls_declared_accessor {
        return None;
    }
    Some(HoistedCompanionStorage {
        index,
        owner,
        bridged: (if writable {
            declared.setter_visibility.is_private()
        } else {
            declared.is_private
        }) && !ir.is_jvm_field_static(index),
    })
}

/// Descriptor of an annotation-interface member. `KClass` is returned as `java.lang.Class`.
fn annotation_member_descriptor(
    parameters: &[crate::types::Ty],
    result: crate::types::Ty,
) -> String {
    let stored =
        crate::jvm::annotation_kclass::annotation_member_jvm_type(jvm_declared_ty(&result));
    method_descriptor(&crate::jvm::method_descriptors::jvm_tys(parameters), stored)
}
