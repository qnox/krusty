//! Selecting the physical read for a property declared by the module being emitted.
//!
//! Local declarations have no class file to query. Their recorded property, field, accessor, and
//! bridge identities are therefore the complete source of the JVM read shape.

use super::*;

impl Emitter<'_> {
    /// How to read property `name` of a class THIS compilation declares — there is no class file to ask,
    /// the IR is the declaration. Inside the declaring class the private backing field is loaded directly,
    /// which is what kotlinc emits there; from outside, the read goes through the accessor. `None` when
    /// `owner` is not a class of this file, or declares no such property.
    pub(super) fn declared_property_read_access(
        &self,
        owner: TypeName,
        name: &str,
        selected_accessor: Option<&str>,
        selected_interface: bool,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        use crate::jvm::inline::PropertyAccess;
        let class = self.ir.classes.iter().find(|c| c.fq_name == owner)?;
        if let Some(index) = class
            .properties
            .iter()
            .position(|property| property.name == name)
        {
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
        let declared_index = class.properties.iter().position(|p| p.name == name);
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
        let accessor = class.methods.iter().copied().find(|&fid| {
            let f = &self.ir.functions[fid as usize];
            let named = f.name == accessor_name
                || f.name
                    .strip_prefix(&accessor_name)
                    .is_some_and(|rest| rest.starts_with('-'));
            let getter_shape = f.params.is_empty()
                || (f.is_static && f.name.ends_with("-impl") && f.params.len() == 1);
            named && getter_shape
        });
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
        // accessor and the field itself is unreachable.
        if let Some(declared) = declared.filter(|p| {
            p.needs_access_bridge && self.static_owner != Some(StaticOwner::Class(class.fq_name))
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
        let static_id = self.ir.jvm_companion_property_static(companion, property)?;
        let storage = self.ir.statics.get(static_id as usize)?;
        let owner = storage.owner?;
        let descriptor = type_descriptor(jvm_declared_ty(&storage.ty));
        let declared = self
            .ir
            .classes
            .iter()
            .find(|class| class.fq_name == companion)?
            .properties
            .get(property as usize)?;
        // A declared accessor is user code. The outer `<clinit>` calls it (through
        // `access$get…` / `access$set…` when it is private) instead of touching the field.
        // The accessor body and the declaration initializer store are the field operations.
        let calls_declared_accessor = if writable {
            declared.modifiers.declared_setter
        } else {
            declared.modifiers.declared_getter
        };
        if calls_declared_accessor {
            return None;
        }
        let private_property = declared.is_private;
        let property_name = declared.name.clone();
        let emitted_by_owner = self.static_owner == Some(StaticOwner::Class(owner));
        if emitted_by_owner || self.ir.is_jvm_field_static(static_id) {
            return Some(PropertyAccess::Field {
                owner,
                name: self.ir.static_field_jvm_name(static_id).to_string(),
                descriptor,
                is_static: true,
            });
        }
        if !private_property {
            return None;
        }
        let (name, descriptor) = if writable {
            (
                format!(
                    "access${}$cp",
                    crate::names::property_setter_name(&property_name)
                ),
                format!("({descriptor})V"),
            )
        } else {
            (
                format!(
                    "access${}$cp",
                    crate::names::property_getter_name(&property_name)
                ),
                format!("(){descriptor}"),
            )
        };
        Some(PropertyAccess::AccessBridge {
            owner,
            name,
            descriptor,
            takes_receiver: false,
            inline_uninitialized_guard: None,
        })
    }
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
