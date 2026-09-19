//! What a property REFERENCE calls when a value class stands on either side of it.
//!
//! A reference crosses the erased `KProperty` boundary: `get` hands back an `Object` and `set`
//! takes one, while the accessor it calls is realized over the carrier. Both ends of that are JVM
//! realization, decided here once the target is already selected — no declaration lookup, no
//! overload resolution, and nothing rebuilt from the property's spelling.

use super::*;

/// Realize every synthesized property reference against the value classes around it.
pub(super) fn realize(
    ir: &mut IrFile,
    callable_under: &Under,
    vc_properties: &HashMap<TypeName, String>,
    erased_top_level_statics: &HashSet<String>,
) {
    // Property references cross the erased `KProperty` Object boundary. A value-class property accessor
    // itself uses the mangled name and carrier descriptor, but `get` must box that carrier and `set` must
    // unbox the incoming value-class object. Record that JVM realization on the already-selected target;
    // no declaration lookup or overload resolution happens here.
    for class in &mut ir.classes {
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        // A reference follows the DECLARATION it calls. A top-level property of value-class type is
        // realized over the carrier above, so its reference mangles like any other; a COMPANION one
        // keeps the boxed convention, and mangling its accessor name while the declaration keeps its
        // plain one would name a method that does not exist. The declaration side says which is
        // which — the erased statics it produced — rather than this deciding it a second time.
        let top_level = reference.static_dispatch;
        if top_level && !erased_top_level_statics.contains(&reference.prop_name) {
            continue;
        }
        let Some(value_class) = reference
            .prop_ty
            .non_null()
            .obj_internal()
            .filter(|name| callable_under.contains_key(name))
        else {
            continue;
        };
        // A specialized generic property (`Pair<UInt, _>::first`) has semantic type `UInt`, but its
        // selected declaration still exposes `getFirst(): Object`. That object is already the boxed
        // value and the declaration is not value-class-mangled. Only a descriptor returning this
        // value class's actual carrier denotes a concrete value-class property accessor.
        if !top_level
            && reference
                .getter_descriptor
                .as_ref()
                .is_some_and(|descriptor| {
                    let physical_ret = descriptor.rsplit_once(')').map(|(_, ret)| ret);
                    let carrier = callable_under
                        .get(&value_class)
                        .map(|underlying| desc(&erase(underlying, &callable_under)));
                    physical_ret
                        .zip(carrier.as_deref())
                        .is_some_and(|(ret, carrier)| ret != carrier)
                })
        {
            continue;
        }
        reference.boxed_value_class = Some(value_class);
        // A TOP-LEVEL property is realized on the file facade, where kotlinc suppresses RETURN
        // mangling: `getTopLevel()I` keeps its plain name while a member's `getZ-a_XrcN0()I` does
        // not. Its SETTER still mangles — a value-class PARAMETER always does — which is why the
        // two accessors of the same property do not agree on it.
        reference.getter_name = vc_mangle(
            &property_getter_name(&reference.prop_name),
            &[],
            &reference.prop_ty,
            &callable_under,
            top_level,
            false,
        );
        // The accessor exchanges the value class's erased CARRIER, never the boxed object. A
        // member or top-level property has no written descriptor, so the one the emitter would
        // synthesize from the property's semantic type (`()LZ;`) names a method that does not
        // exist — the declaration returns `()I`. Synthesize the carrier form here, where the
        // underlying type is in hand.
        let carrier = callable_under
            .get(&value_class)
            .map(|underlying| desc(&erase(underlying, &callable_under)));
        match (reference.getter_descriptor.as_mut(), carrier.as_deref()) {
            (Some(descriptor), _) => *descriptor = erase_descriptor(descriptor, &callable_under),
            (None, Some(carrier)) => reference.getter_descriptor = Some(format!("(){carrier}")),
            (None, None) => {}
        }
        if reference.setter_name.is_some() {
            reference.setter_name = Some(vc_mangle(
                &crate::names::property_setter_name(&reference.prop_name),
                std::slice::from_ref(&reference.prop_ty),
                &Ty::Unit,
                &callable_under,
                top_level,
                false,
            ));
        }
        match (
            reference.setter_descriptor.as_mut(),
            reference.setter_name.is_some(),
            carrier.as_deref(),
        ) {
            (Some(descriptor), _, _) => {
                *descriptor = erase_descriptor(descriptor, &callable_under);
            }
            (None, true, Some(carrier)) => {
                reference.setter_descriptor = Some(format!("({carrier})V"));
            }
            (None, _, _) => {}
        }
    }

    // The RECEIVER side of the same boundary. A property whose reference receiver is a value class
    // — a member of one (`Z::xx`) or an extension on one (`val Z.xx`) — has its accessor realized
    // STATICALLY over the erased carrier: `Z.getXx-impl(I)I`, `ExtKt.getXx-IQRRRT4(I)I`. The
    // reference's `get(Object)` therefore unboxes its argument before the call, exactly as
    // kotlinc's does. Without this the reference named an instance accessor that is not declared
    // anywhere, and the program failed at its first `get` with a `NoSuchMethodError`.
    // Method identities of every value-class owner, by final (already mangled) name: the loop below
    // borrows `ir.classes` mutably and still has to name the exact accessor a bridge belongs to.
    let mut value_class_methods: std::collections::HashMap<(TypeName, String), u32> =
        std::collections::HashMap::new();
    for class in ir
        .classes
        .iter()
        .filter(|c| callable_under.contains_key(&c.fq_name))
    {
        for &fid in &class.methods {
            if let Some(function) = ir.functions.get(fid as usize) {
                value_class_methods.insert((class.fq_name, function.name.clone()), fid);
            }
        }
    }
    let mut access_bridges: Vec<u32> = Vec::new();
    for class in &mut ir.classes {
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        if reference.static_dispatch {
            continue;
        }
        let Some(receiver) = reference
            .owner_internal
            .filter(|owner| callable_under.contains_key(owner))
        else {
            continue;
        };
        // A value class's own UNDERLYING property is the exception: reading it IS the unbox, and
        // its accessor stays an ordinary instance getter on the box (`Z.getX()I`, which is also
        // the signature the reference reports) rather than a static realization over the carrier.
        // Such a reference needs no rewrite at all.
        if vc_properties
            .get(&receiver)
            .is_some_and(|underlying| *underlying == reference.prop_name)
        {
            continue;
        }
        let Some(carrier) = callable_under
            .get(&receiver)
            .map(|underlying| erase(underlying, &callable_under))
        else {
            continue;
        };
        // An EXTENSION accessor already names its receiver in its descriptor and is mangled by the
        // hash; a MEMBER accessor names none and takes the structural `-impl` suffix, with the
        // carrier prepended after mangling — the same two rules the declaration side applies.
        //
        // The role is what the reference RECORDED, never what `ext_facade` looks like: that field is
        // `Some` for an extension AND for a private member reached through an `access$…` bridge, so
        // reading it as "this is an extension" rebuilt `getXx-<hash>(I)I` for a member whose
        // declaration is `getXx-impl(I)I`, and the program failed at its first `get` with a
        // `NoSuchMethodError`.
        let extension = match reference.accessor_role {
            crate::ir::PropertyAccessorRole::Extension => true,
            crate::ir::PropertyAccessorRole::Member => false,
            // A private member's accessor is realized statically over the carrier exactly like any
            // other member of the value class; krusty publishes it directly rather than behind a
            // bridge, so the member rule is the one that names the declaration.
            crate::ir::PropertyAccessorRole::AccessBridge => false,
        };
        let boxed_receiver = Ty::obj_name(receiver);
        let declared_params: &[Ty] = if extension {
            std::slice::from_ref(&boxed_receiver)
        } else {
            &[]
        };
        let mut getter = vc_mangle(
            &property_getter_name(&reference.prop_name),
            declared_params,
            &reference.prop_ty,
            &callable_under,
            extension,
            false,
        );
        if !extension && getter == property_getter_name(&reference.prop_name) {
            getter.push_str("-impl");
        }
        let physical_ret = reference
            .getter_descriptor
            .as_deref()
            .and_then(|descriptor| descriptor.rsplit_once(')').map(|(_, ret)| ret.to_string()))
            .unwrap_or_else(|| desc(&erase(&reference.prop_ty, &callable_under)));
        reference.getter_name = getter;
        reference.getter_descriptor = Some(format!("({}){physical_ret}", desc(&carrier)));
        if let Some(setter) = reference.setter_name.as_mut() {
            let value = reference.prop_ty;
            let mut params = declared_params.to_vec();
            params.push(value);
            let mut mangled = vc_mangle(
                &crate::names::property_setter_name(&reference.prop_name),
                &params,
                &Ty::Unit,
                &callable_under,
                extension,
                false,
            );
            if !extension && mangled == crate::names::property_setter_name(&reference.prop_name) {
                mangled.push_str("-impl");
            }
            *setter = mangled;
            reference.setter_descriptor = Some(format!(
                "({}{})V",
                desc(&carrier),
                desc(&erase(&value, &callable_under))
            ));
        }
        // A PRIVATE member keeps its accessor private, as kotlinc does, and the reference reaches
        // it through the synthetic bridge beside it: `access$getXx-impl(I)I`, not the declaration.
        // The name is the DECLARATION's mangled one with the prefix — the bridge exists for this
        // exact accessor — so the reference still names a method the class really has, which is
        // what publishing the accessor instead only appeared to achieve.
        if reference.accessor_role == crate::ir::PropertyAccessorRole::AccessBridge {
            for name in
                std::iter::once(&mut reference.getter_name).chain(reference.setter_name.as_mut())
            {
                if let Some(target) = value_class_methods.get(&(receiver, name.clone())) {
                    access_bridges.push(*target);
                }
                name.insert_str(0, "access$");
            }
        }
        // A member's accessor is static on the value class itself; an extension's already routes
        // through its facade. Neither changes `ext_facade`, which the reference's reflection owner
        // and its top-level flag are read from — a member of a value class is still a MEMBER
        // reference (`ldc Z.class`, flags 0), and only the physical call shape changes.
        reference.unboxed_receiver_value_class = Some(receiver);
    }
    ir.function_reference_access_bridges.extend(access_bridges);
}
