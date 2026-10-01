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
        let interface = is_jvm_interface(class) || selected_interface;
        // A property that DECLARES an accessor (computed, delegated, or `field`-using) is always read
        // through it — the accessor is user code, and a direct field load would skip it. Only a plain
        // backing-field property may be read directly, and only from inside the declaring class.
        let declared = class.properties.iter().find(|p| p.name == name);
        let direct_field = self.direct_field_access(class, declared, false);
        if let Some(getter) = declared.and_then(|p| p.getter) {
            let f = &self.ir.functions[getter as usize];
            // Another class reads a private getter through its bridge, kotlinc's `access$<getter>`.
            if self.reaches_through_bridge(class.fq_name, getter) {
                return Some(access_bridges::private_member_read_access(
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
                descriptor: ir_method_desc(&f.params, &f.ret),
                is_static: f.is_static,
                is_interface: interface,
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
        let accessor = class.methods.iter().find_map(|&fid| {
            let f = &self.ir.functions[fid as usize];
            let named = f.name == accessor_name
                || f.name
                    .strip_prefix(&accessor_name)
                    .is_some_and(|rest| rest.starts_with('-'));
            let getter_shape = f.params.is_empty()
                || (f.is_static && f.name.ends_with("-impl") && f.params.len() == 1);
            (named && getter_shape).then_some(f)
        });
        if let Some(accessor) = accessor.filter(|_| !direct_field || field.is_none()) {
            return Some(PropertyAccess::Accessor {
                owner,
                name: accessor.name.clone(),
                descriptor: ir_method_desc(&accessor.params, &accessor.ret),
                is_static: accessor.is_static,
                is_interface: interface,
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
        let Some(field) = field else {
            let ty = declared.map(|p| p.ty)?;
            return Some(PropertyAccess::Accessor {
                owner,
                name: accessor_name,
                descriptor: ir_method_desc(&[], &stored_value_ty(ty)),
                is_static: false,
                is_interface: interface,
            });
        };
        // Outside the declaring class the backing field is private, so the read goes through the
        // accessor — the one synthesized for this declaration, which carries no IR method of its own.
        if !direct_field {
            return Some(PropertyAccess::Accessor {
                owner,
                name: accessor_name,
                descriptor: method_descriptor(
                    &[],
                    declared
                        .map(|property| declared_property_accessor_jvm(self.ir, property, field))
                        .unwrap_or_else(|| jvm_declared_ty(&field.ty)),
                ),
                is_static: false,
                is_interface: interface,
            });
        }
        Some(PropertyAccess::Field {
            owner,
            name: instance_field_jvm_name(self.ir, class, field),
            descriptor: type_descriptor(jvm_declared_ty(&field.ty)),
            // A static-storage object's backing fields are JVM statics (kotlinc's shape).
            is_static: static_storage(self.ir, class),
        })
    }
}
