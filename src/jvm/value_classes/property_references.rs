//! What a property REFERENCE calls when a value class stands on either side of it.
//!
//! A reference crosses the erased `KProperty` boundary: `get` hands back an `Object` and `set`
//! takes one, while the accessor it calls is realized over the carrier. Both ends of that are JVM
//! realization, decided here once the target is already selected — no declaration lookup, no
//! overload resolution, and nothing rebuilt from the property's spelling.

use super::*;

/// Realize every synthesized property reference against the value classes around it.
/// Whether the property this reference names is realized over its value class's carrier.
///
/// Derived from the reference's own recorded declaration facts and the same `erase` rule the
/// declaration side applies, so the two sides agree without either publishing a list of names: a
/// name-keyed join cannot see a sibling file's declaration, and matches an unrelated same-spelled
/// one in this file.
fn facade_storage_is_carrier_erased(reference: &crate::ir::PropRef, under: &Under) -> bool {
    reference.facade_storage && erase(&reference.prop_ty, under) != reference.prop_ty
}

pub(super) fn realize(
    ir: &mut IrFile,
    callable_under: &Under,
    vc_properties: &HashMap<TypeName, String>,
) -> bool {
    // Property references cross the erased `KProperty` Object boundary. A value-class property accessor
    // itself uses the mangled name and carrier descriptor, but `get` must box that carrier and `set` must
    // unbox the incoming value-class object. Record that JVM realization on the already-selected target;
    // no declaration lookup or overload resolution happens here.
    for class in &mut ir.classes {
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        // A reference follows the DECLARATION it calls. A COMPANION-associated property is also
        // static-dispatch but keeps the boxed convention, and mangling its accessor name while the
        // declaration keeps its plain one would name a method that does not exist — so the
        // reference's own recorded facts say which shape this is, rather than this pass deciding a
        // second time or looking the declaration up by spelling.
        let top_level = reference.static_dispatch;
        if top_level && !reference.facade_storage {
            continue;
        }
        let carrier_erased =
            !top_level || facade_storage_is_carrier_erased(reference, callable_under);
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
        if !carrier_erased {
            // The storage stayed boxed — a nullable value class over a scalar carrier. Its
            // accessors exchange the box, so nothing about this reference's descriptors changes;
            // only the SETTER's name does, because its value-class PARAMETER mangles whether or not
            // the field behind it holds the carrier. Tying the two together left the reference
            // naming an unmangled setter the facade does not declare.
            let declared_setter = reference.declared_setter_name.clone();
            if let (Some(setter), Some(declared_setter)) =
                (reference.setter_name.as_mut(), declared_setter)
            {
                // `is_file_class` exempts a value-class RESULT from the hash, never a value-class
                // PARAMETER, and the declaration side names this setter with the same `false`.
                *setter = vc_mangle(
                    &declared_setter,
                    std::slice::from_ref(&reference.prop_ty),
                    &Ty::Unit,
                    &callable_under,
                    false,
                    false,
                );
            }
            continue;
        }
        reference.boxed_value_class = Some(value_class);
        // A TOP-LEVEL property is realized on the file facade, where kotlinc suppresses RETURN
        // mangling: `getTopLevel()I` keeps its plain name while a member's `getZ-a_XrcN0()I` does
        // not. Its SETTER still mangles — a value-class PARAMETER always does — which is why the
        // two accessors of the same property do not agree on it.
        reference.getter_name = vc_mangle(
            &reference.declared_getter_name.clone(),
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
        let declared_setter = reference.declared_setter_name.clone();
        if let (true, Some(declared_setter)) = (reference.setter_name.is_some(), declared_setter) {
            reference.setter_name = Some(vc_mangle(
                &declared_setter,
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
        // The base name is the one the DECLARATION carries, which selection already resolved —
        // `@get:JvmName("readY")` is `readY` there whatever the property is spelled. Rebuilding it
        // from `prop_name` discards that answer and names a method the class does not declare.
        let declared_getter = reference.declared_getter_name.clone();
        let mut getter = vc_mangle(
            &declared_getter,
            declared_params,
            &reference.prop_ty,
            &callable_under,
            extension,
            false,
        );
        if !extension && getter == declared_getter {
            getter.push_str("-impl");
        }
        let physical_ret = reference
            .getter_descriptor
            .as_deref()
            .and_then(|descriptor| descriptor.rsplit_once(')').map(|(_, ret)| ret.to_string()))
            .unwrap_or_else(|| desc(&erase(&reference.prop_ty, &callable_under)));
        reference.getter_name = getter;
        reference.getter_descriptor = Some(format!("({}){physical_ret}", desc(&carrier)));
        let declared_setter = reference.declared_setter_name.clone();
        if let (Some(setter), Some(declared_setter)) =
            (reference.setter_name.as_mut(), declared_setter)
        {
            let value = reference.prop_ty;
            let mut params = declared_params.to_vec();
            params.push(value);
            let mut mangled = vc_mangle(
                &declared_setter,
                &params,
                &Ty::Unit,
                &callable_under,
                extension,
                false,
            );
            if !extension && mangled == declared_setter {
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
                // The bridge exists for one exact accessor, and that accessor is the method this
                // realization just named. Prefixing `access$` anyway when no such method is there
                // produced a reference to something nothing declares — the very failure this pass
                // exists to remove — so a miss refuses the file instead.
                let Some(target) = value_class_methods.get(&(receiver, name.clone())) else {
                    crate::trace_compiler!(
                        "value_classes",
                        "no access bridge target for {}.{name}",
                        receiver.render(),
                    );
                    return false;
                };
                access_bridges.push(*target);
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
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference whose target is a member of `Vault`, a value class over `Int`, named by an
    /// accessor the declaration does NOT spell `get<Property>`.
    fn reference_to(declared_getter: &str) -> (IrFile, Under) {
        let vault = crate::types::type_name("Vault");
        let mut ir = IrFile::default();
        let mut holder = crate::ir::IrClass::synthetic(crate::types::type_name("Ref$0"));
        holder.prop_ref = Some(crate::ir::PropRef {
            owner_internal: Some(vault),
            call_owner_internal: Some(vault),
            prop_name: "tally".to_string(),
            declared_getter_name: declared_getter.to_string(),
            declared_setter_name: None,
            facade_storage: false,
            getter_name: declared_getter.to_string(),
            getter_descriptor: None,
            setter_name: None,
            setter_descriptor: None,
            boxed_value_class: None,
            unboxed_receiver_value_class: None,
            owner_is_interface: false,
            prop_ty: Ty::Int,
            bound: false,
            static_dispatch: false,
            mutable: false,
            ext_facade: None,
            accessor_role: crate::ir::PropertyAccessorRole::Member,
        });
        ir.add_class(holder);
        let under = Under::from_iter([(vault, Ty::Int)]);
        (ir, under)
    }

    /// The realization mangles the accessor name the DECLARATION carries, never one rebuilt from
    /// the property's spelling.
    ///
    /// Selection resolves that name — a `@get:JvmName("readTally")` accessor is `readTally` there
    /// whatever the property is called — and rebuilding it from `prop_name` names a method the
    /// value class does not declare, which links to nothing. The two fixture names are deliberately
    /// unrelated to each other, so only reading the recorded one can produce this answer.
    #[test]
    fn the_realization_mangles_the_recorded_accessor_name_not_the_property_spelling() {
        let (mut ir, under) = reference_to("readTally");
        assert!(realize(&mut ir, &under, &HashMap::new()));
        let reference = ir.classes[0].prop_ref.as_ref().expect("the reference");
        assert_eq!(
            reference.getter_name, "readTally-impl",
            "the recorded name takes the value class's structural suffix",
        );

        // The contrast, to show the assertion above is not satisfied by the spelling by accident.
        let (mut ir, under) = reference_to("getTally");
        assert!(realize(&mut ir, &under, &HashMap::new()));
        assert_eq!(
            ir.classes[0]
                .prop_ref
                .as_ref()
                .expect("the reference")
                .getter_name,
            "getTally-impl",
        );
    }

    /// An access bridge is reached through the method the realization just named. When the value
    /// class declares no such method the reference would name something nothing declares, which is
    /// the exact failure this pass exists to remove — so the file is refused instead of a plain
    /// `access$` prefix being put in front of a name that links to nothing.
    #[test]
    fn a_missing_access_bridge_target_refuses_the_file() {
        let (mut ir, under) = reference_to("readTally");
        ir.classes[0]
            .prop_ref
            .as_mut()
            .expect("the reference")
            .accessor_role = crate::ir::PropertyAccessorRole::AccessBridge;
        assert!(
            !realize(&mut ir, &under, &HashMap::new()),
            "no declaration to bridge to, so the realization refuses",
        );
    }
}
