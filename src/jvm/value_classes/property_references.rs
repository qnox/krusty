//! What a property REFERENCE calls when a value class stands on either side of it.
//!
//! A reference crosses the erased `KProperty` boundary: `get` hands back an `Object` and `set`
//! takes one, while the accessor it calls is realized over the carrier. Both ends of that are JVM
//! realization, decided here once the target is already selected — no declaration lookup, no
//! overload resolution, and nothing rebuilt from the property's spelling. Every fact this pass
//! needs about the selected accessor was recorded where the selection happened, in
//! [`PropertyReferenceRealization`]; this pass reads them and writes back the ones it changes.

use super::*;

use crate::jvm::property_references::{
    PropertyAccessorRole, PropertyReferenceRealization, PropertyReferenceRealizations,
};

/// Whether the property this reference names is realized over its value class's carrier.
///
/// Derived from the reference's own recorded declaration facts and the same `erase` rule the
/// declaration side applies, so the two sides agree without either publishing a list of names: a
/// name-keyed join cannot see a sibling file's declaration, and matches an unrelated same-spelled
/// one in this file.
fn facade_storage_is_carrier_erased(
    reference: &crate::ir::PropRef,
    realization: &PropertyReferenceRealization,
    under: &Under,
) -> bool {
    realization.facade_storage && erase(&reference.prop_ty, under) != reference.prop_ty
}

/// Whether the selected getter physically hands back something OTHER than `value_class`'s carrier.
///
/// A specialized generic property (`Pair<UInt, _>::first`) has semantic type `UInt`, but its
/// selected declaration still exposes `getFirst(): Object`. That object is already the boxed value
/// and the declaration is not value-class-mangled, so no realization applies to it. The answer
/// comes from the physical return the selection RECORDED; reading it back out of a rendered
/// descriptor would make a rendering the authority over a declaration.
fn getter_bypasses_the_carrier(
    realization: &PropertyReferenceRealization,
    value_class: TypeName,
    under: &Under,
) -> bool {
    let Some(physical) = realization.physical_getter_ret else {
        return false;
    };
    under
        .get(&value_class)
        .map(|underlying| erase(underlying, under))
        .is_some_and(|carrier| desc(&physical) != desc(&carrier))
}

pub(super) fn realize(
    ir: &mut IrFile,
    callable_under: &Under,
    realizations: &mut PropertyReferenceRealizations,
) -> bool {
    // Property references cross the erased `KProperty` Object boundary. A value-class property accessor
    // itself uses the mangled name and carrier descriptor, but `get` must box that carrier and `set` must
    // unbox the incoming value-class object. Record that JVM realization on the already-selected target;
    // no declaration lookup or overload resolution happens here.
    for class in &mut ir.classes {
        let owner = class.fq_name;
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        let Some(realization) = realizations.get_mut(owner) else {
            continue;
        };
        // A reference follows the DECLARATION it calls. A COMPANION-associated property is also
        // static-dispatch but keeps the boxed convention, and mangling its accessor name while the
        // declaration keeps its plain one would name a method that does not exist — so the
        // reference's own recorded facts say which shape this is, rather than this pass deciding a
        // second time or looking the declaration up by spelling.
        let top_level = reference.static_dispatch;
        if top_level && !realization.facade_storage {
            continue;
        }
        let carrier_erased =
            !top_level || facade_storage_is_carrier_erased(reference, realization, callable_under);
        let Some(value_class) = reference
            .prop_ty
            .non_null()
            .obj_internal()
            .filter(|name| callable_under.contains_key(name))
        else {
            continue;
        };
        if !top_level && getter_bypasses_the_carrier(realization, value_class, callable_under) {
            continue;
        }
        if !carrier_erased {
            // The storage stayed boxed — a nullable value class over a scalar carrier. Its
            // accessors exchange the box, so nothing about this reference's descriptors changes;
            // only the SETTER's name does, because its value-class PARAMETER mangles whether or not
            // the field behind it holds the carrier. Tying the two together left the reference
            // naming an unmangled setter the facade does not declare.
            if !realization.accessor_names_are_physical {
                if let (Some(setter), Some(declared_setter)) = (
                    reference.setter_name.as_mut(),
                    realization.declared_setter_name.as_deref(),
                ) {
                    // `is_file_class` exempts a value-class RESULT from the hash, never a
                    // value-class PARAMETER, and the declaration side names this setter with the
                    // same `false`.
                    *setter = vc_mangle(
                        declared_setter,
                        std::slice::from_ref(&reference.prop_ty),
                        &Ty::Unit,
                        callable_under,
                        false,
                        false,
                    );
                }
            }
            continue;
        }
        realization.boxed_value_class = Some(value_class);
        // A TOP-LEVEL property is realized on the file facade, where kotlinc suppresses RETURN
        // mangling: `getTopLevel()I` keeps its plain name while a member's `getZ-a_XrcN0()I` does
        // not. Its SETTER still mangles — a value-class PARAMETER always does — which is why the
        // two accessors of the same property do not agree on it.
        if !realization.accessor_names_are_physical {
            reference.getter_name = vc_mangle(
                &realization.declared_getter_name,
                &[],
                &reference.prop_ty,
                callable_under,
                top_level,
                false,
            );
        }
        // The accessor exchanges the value class's erased CARRIER, never the boxed object. A
        // member or top-level property has no written descriptor, so the one the emitter would
        // synthesize from the property's semantic type (`()LZ;`) names a method that does not
        // exist — the declaration returns `()I`. Synthesize the carrier form here, where the
        // underlying type is in hand.
        let carrier = callable_under
            .get(&value_class)
            .map(|underlying| erase(underlying, callable_under));
        // Whatever this arm decides the accessor physically returns is recorded beside the
        // descriptor it writes, so the receiver pass below reads a fact rather than parsing one
        // back out of the rendering this pass just produced.
        match (reference.getter_descriptor.as_mut(), carrier) {
            (Some(descriptor), _) => {
                *descriptor = erase_descriptor(descriptor, callable_under);
                realization.physical_getter_ret = realization
                    .physical_getter_ret
                    .map(|physical| erase(&physical, callable_under));
            }
            (None, Some(carrier)) => {
                reference.getter_descriptor = Some(format!("(){}", desc(&carrier)));
                realization.physical_getter_ret = Some(carrier);
            }
            (None, None) => {}
        }
        if !realization.accessor_names_are_physical {
            if let (true, Some(declared_setter)) = (
                reference.setter_name.is_some(),
                realization.declared_setter_name.as_deref(),
            ) {
                reference.setter_name = Some(vc_mangle(
                    declared_setter,
                    std::slice::from_ref(&reference.prop_ty),
                    &Ty::Unit,
                    callable_under,
                    top_level,
                    false,
                ));
            }
        }
        match (
            reference.setter_descriptor.as_mut(),
            reference.setter_name.is_some(),
            carrier,
        ) {
            (Some(descriptor), _, _) => {
                *descriptor = erase_descriptor(descriptor, callable_under);
            }
            (None, true, Some(carrier)) => {
                reference.setter_descriptor = Some(format!("({})V", desc(&carrier)));
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
    //
    // The spellings of the accessors an `access$…` bridge forwards to, read up front by the
    // IDENTITY the selection recorded: the loop below borrows `ir.classes` mutably and still has
    // to name the exact method each bridge belongs to.
    let bridged_accessor_names: std::collections::HashMap<crate::ir::FunId, String> = realizations
        .accessor_functions()
        .filter_map(|function| {
            ir.functions
                .get(function as usize)
                .map(|declaration| (function, declaration.name.clone()))
        })
        .collect();
    let mut access_bridges: Vec<crate::ir::FunId> = Vec::new();
    for class in &mut ir.classes {
        let owner = class.fq_name;
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        let Some(realization) = realizations.get_mut(owner) else {
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
        // Such a reference needs no rewrite at all. The selection recorded that the property is
        // the class's storage; a value class's underlying property has no spelling to recognize it
        // by, and the sole-field position that does identify it belongs to the declaration.
        if realization.declares_value_class_storage {
            continue;
        }
        let Some(carrier) = callable_under
            .get(&receiver)
            .map(|underlying| erase(underlying, callable_under))
        else {
            continue;
        };
        // An EXTENSION accessor already names its receiver in its descriptor and is mangled by the
        // hash; a MEMBER accessor names none and takes the structural `-impl` suffix, with the
        // carrier prepended after mangling — the same two rules the declaration side applies.
        //
        // The role is what the selection RECORDED, never what `ext_facade` looks like: that field is
        // `Some` for an extension AND for a private member reached through an `access$…` bridge, so
        // reading it as "this is an extension" rebuilt `getXx-<hash>(I)I` for a member whose
        // declaration is `getXx-impl(I)I`, and the program failed at its first `get` with a
        // `NoSuchMethodError`.
        let extension = match realization.accessor_role {
            PropertyAccessorRole::Extension => true,
            PropertyAccessorRole::Member => false,
            // A private member's accessor is realized statically over the carrier exactly like any
            // other member of the value class; krusty publishes it directly rather than behind a
            // bridge, so the member rule is the one that names the declaration.
            PropertyAccessorRole::AccessBridge => false,
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
        let declared_getter = realization.declared_getter_name.clone();
        let mut getter = declared_getter.clone();
        if !realization.accessor_names_are_physical {
            getter = vc_mangle(
                &declared_getter,
                declared_params,
                &reference.prop_ty,
                callable_under,
                extension,
                false,
            );
            if !extension && getter == declared_getter {
                getter.push_str("-impl");
            }
        }
        let physical_ret = realization
            .physical_getter_ret
            .map(|physical| desc(&physical))
            .unwrap_or_else(|| desc(&erase(&reference.prop_ty, callable_under)));
        reference.getter_name = getter;
        reference.getter_descriptor = Some(format!("({}){physical_ret}", desc(&carrier)));
        if let (Some(setter), Some(declared_setter)) = (
            reference.setter_name.as_mut(),
            realization.declared_setter_name.as_deref(),
        ) {
            let value = reference.prop_ty;
            let mut params = declared_params.to_vec();
            params.push(value);
            let mut mangled = declared_setter.to_string();
            if !realization.accessor_names_are_physical {
                mangled = vc_mangle(
                    declared_setter,
                    &params,
                    &Ty::Unit,
                    callable_under,
                    extension,
                    false,
                );
                if !extension && mangled == declared_setter {
                    mangled.push_str("-impl");
                }
            }
            *setter = mangled;
            reference.setter_descriptor = Some(format!(
                "({}{})V",
                desc(&carrier),
                desc(&erase(&value, callable_under))
            ));
        }
        // A PRIVATE member keeps its accessor private, as kotlinc does, and the reference reaches
        // it through the synthetic bridge beside it: `access$getXx-impl(I)I`, not the declaration.
        // The bridge exists for one exact accessor — the function the selection recorded — and is
        // named after it, so the reference names a method the class really has instead of a
        // spelling something is hoped to answer to.
        if realization.accessor_role == PropertyAccessorRole::AccessBridge {
            // A read-only reference simply has no setter half; the selection recorded each
            // accessor's function beside the name the reference carries for it.
            let accessors = [
                (
                    Some(&mut reference.getter_name),
                    realization.getter_function,
                ),
                (reference.setter_name.as_mut(), realization.setter_function),
            ];
            for (name, function) in accessors {
                let Some(name) = name else {
                    continue;
                };
                // A bridge that has no recorded target would leave the reference naming something
                // nothing declares — the very failure this pass exists to remove — so a miss
                // refuses the file instead of a plain `access$` prefix being put in front of it.
                let Some(target) = function.and_then(|function| {
                    bridged_accessor_names
                        .get(&function)
                        .map(|target| (function, target))
                }) else {
                    crate::trace_compiler!(
                        "value_classes",
                        "no access bridge target for {}.{name}",
                        receiver.render(),
                    );
                    return false;
                };
                access_bridges.push(target.0);
                *name = format!("access${}", target.1);
            }
        }
        // A member's accessor is static on the value class itself; an extension's already routes
        // through its facade. Neither changes `ext_facade`, which the reference's reflection owner
        // and its top-level flag are read from — a member of a value class is still a MEMBER
        // reference (`ldc Z.class`, flags 0), and only the physical call shape changes.
        realization.unboxed_receiver_value_class = Some(receiver);
    }
    ir.function_reference_access_bridges.extend(access_bridges);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference whose target is a member of `Vault`, a value class over `Int`, named by an
    /// accessor the declaration does NOT spell `get<Property>`.
    ///
    /// The accessor is a real function of the file, so the realization can name it by identity —
    /// which is what an access bridge is required to do.
    fn reference_to(
        declared_getter: &str,
    ) -> (IrFile, Under, PropertyReferenceRealizations, TypeName) {
        let vault = crate::types::type_name("Vault");
        let mut ir = IrFile::default();
        let accessor = ir.functions.len() as crate::ir::FunId;
        ir.functions.push(crate::ir::IrFunction {
            name: format!("{declared_getter}-impl"),
            params: vec![Ty::Int],
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut holder = crate::ir::IrClass::synthetic(crate::types::type_name("Ref$0"));
        let reference = holder.fq_name;
        holder.prop_ref = Some(crate::ir::PropRef {
            owner_internal: Some(vault),
            call_owner_internal: Some(vault),
            prop_name: "tally".to_string(),
            getter_name: declared_getter.to_string(),
            getter_descriptor: None,
            setter_name: None,
            setter_descriptor: None,
            owner_is_interface: false,
            prop_ty: Ty::Int,
            bound: false,
            static_dispatch: false,
            mutable: false,
            ext_facade: None,
        });
        ir.add_class(holder);
        let mut realizations = PropertyReferenceRealizations::default();
        realizations.record(
            reference,
            PropertyReferenceRealization {
                declared_getter_name: declared_getter.to_string(),
                declared_setter_name: None,
                facade_storage: false,
                accessor_role: PropertyAccessorRole::Member,
                getter_function: Some(accessor),
                setter_function: None,
                physical_getter_ret: None,
                declares_value_class_storage: false,
                accessor_names_are_physical: false,
                boxed_value_class: None,
                unboxed_receiver_value_class: None,
            },
        );
        let under = Under::from_iter([(vault, Ty::Int)]);
        (ir, under, realizations, reference)
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
        let (mut ir, under, mut realizations, _) = reference_to("readTally");
        assert!(realize(&mut ir, &under, &mut realizations));
        let reference = ir.classes[0].prop_ref.as_ref().expect("the reference");
        assert_eq!(
            reference.getter_name, "readTally-impl",
            "the recorded name takes the value class's structural suffix",
        );

        // The contrast, to show the assertion above is not satisfied by the spelling by accident.
        let (mut ir, under, mut realizations, _) = reference_to("getTally");
        assert!(realize(&mut ir, &under, &mut realizations));
        assert_eq!(
            ir.classes[0]
                .prop_ref
                .as_ref()
                .expect("the reference")
                .getter_name,
            "getTally-impl",
        );
    }

    /// An access bridge is named after the exact accessor the SELECTION recorded, not after a name
    /// this pass rebuilt: the bridge exists for that one declaration.
    #[test]
    fn an_access_bridge_is_named_after_the_accessor_the_selection_recorded() {
        let (mut ir, under, mut realizations, reference) = reference_to("readTally");
        realizations
            .get_mut(reference)
            .expect("the realization")
            .accessor_role = PropertyAccessorRole::AccessBridge;
        assert!(realize(&mut ir, &under, &mut realizations));
        assert_eq!(
            ir.classes[0]
                .prop_ref
                .as_ref()
                .expect("the reference")
                .getter_name,
            "access$readTally-impl",
        );
        assert_eq!(
            ir.function_reference_access_bridges
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0],
            "the bridge is planned for the recorded accessor itself",
        );
    }

    /// An access bridge with no recorded accessor refuses the file. The reference would otherwise
    /// name something nothing declares, which is the exact failure this pass exists to remove.
    #[test]
    fn a_missing_access_bridge_target_refuses_the_file() {
        let (mut ir, under, mut realizations, reference) = reference_to("readTally");
        let realization = realizations.get_mut(reference).expect("the realization");
        realization.accessor_role = PropertyAccessorRole::AccessBridge;
        realization.getter_function = None;
        assert!(
            !realize(&mut ir, &under, &mut realizations),
            "no declaration to bridge to, so the realization refuses",
        );
    }
}
