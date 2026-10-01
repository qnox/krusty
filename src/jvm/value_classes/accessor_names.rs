//! JVM names of synthesized value-class property accessors.
//!
//! A property the backend synthesizes (`getter` absent, backing field present) has no callable
//! of its own. Its JVM spelling is a fact of that property. A call is renamed only when an
//! earlier pass bound that exact call to the property; a missing expression or a callee that is
//! not that virtual accessor is a broken binding. A property-accessor bridge is retargeted only
//! when its selected implementation is this property. Function bridges and a same-spelled
//! accessor of another declaration keep their targets.

use super::member_names::vc_mangle;
use super::Under;
use crate::ir::{BridgeAccessorRole, BridgeKind, Callee, IrExpr, IrFile, IrLocalPropertyLayout};
use crate::jvm::names::property_getter_name;
use crate::names::property_setter_name;
use crate::types::Ty;

pub(super) fn stamp_synthesized(ir: &mut IrFile, under: &Under) {
    for class_index in 0..ir.classes.len() {
        let properties: Vec<(usize, String, Ty)> = ir.classes[class_index]
            .properties
            .iter()
            .enumerate()
            .filter(|(_, property)| property.getter.is_none() && property.backing_field.is_some())
            .map(|(index, property)| (index, property.name.clone(), property.ty))
            .collect();
        for (index, name, ty) in properties {
            let declared_value_class = ty
                .non_null()
                .obj_internal()
                .is_some_and(|fq_name| under.contains_key(&fq_name));
            // The property's own accessor is mangled only when its own type is a value class.
            // When it merely overrides a value-class property (`override val p: Nothing?`), the
            // own accessor keeps the plain spelling and its bridge carries the supertype's name.
            if !declared_value_class {
                continue;
            }
            let plain_getter = property_getter_name(&name);
            let getter = vc_mangle(&plain_getter, &[], &ty, under, false, false);
            let plain_setter = property_setter_name(&name);
            let setter = vc_mangle(
                &plain_setter,
                std::slice::from_ref(&ty),
                &Ty::Unit,
                under,
                false,
                false,
            );
            rename_bound_calls(ir, class_index as u32, index as u32, &getter);
            retarget_property_bridges(ir, class_index, index as u32, &getter, &setter);
            let property = &mut ir.classes[class_index].properties[index];
            property.getter_jvm_name = Some(getter);
            property.setter_jvm_name = Some(setter);
        }
    }
}

fn rename_bound_calls(ir: &mut IrFile, class: u32, property: u32, getter: &str) {
    let calls = ir
        .synthesized_accessor_calls
        .iter()
        .filter(|(_, binding)| binding.class == class && binding.property == property)
        .map(|(call, _)| *call)
        .collect::<Vec<_>>();
    for call in calls {
        let Some(expression) = ir.exprs.get_mut(call as usize) else {
            panic!("synthesized accessor binding {call} does not name an expression");
        };
        let IrExpr::Call {
            callee: Callee::Virtual { name, .. },
            ..
        } = expression
        else {
            panic!("synthesized accessor binding {call} is not a virtual call");
        };
        *name = getter.to_string();
    }
}

fn retarget_property_bridges(
    ir: &mut IrFile,
    class_index: usize,
    property_index: u32,
    getter: &str,
    setter: &str,
) {
    for bridge in &mut ir.classes[class_index].bridges {
        let Some(implementation) = bridge.property_implementation else {
            continue;
        };
        // An identity this file did not lay out is not this member. Spelling is not a substitute.
        let Some(layout) = ir.local_property_layouts.get(&implementation.property) else {
            continue;
        };
        let IrLocalPropertyLayout::Member {
            class, property, ..
        } = layout
        else {
            panic!("a property-accessor bridge delegates to a property that is not a class member");
        };
        if *class as usize != class_index || *property != property_index {
            continue;
        }
        let mangled = match (bridge.kind, implementation.accessor) {
            (BridgeKind::PropertyGetter, BridgeAccessorRole::Getter) => getter,
            (BridgeKind::PropertySetter, BridgeAccessorRole::Setter) => setter,
            _ => panic!(
                "property bridge kind {:?} does not match accessor {:?}",
                bridge.kind, implementation.accessor
            ),
        };
        bridge.target_name = Some(mangled.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{rename_bound_calls, retarget_property_bridges};
    use crate::fir::PropertyId;
    use crate::ir::{
        Bridge, BridgeAccessorRole, BridgeKind, BridgePropertyImplementation, Callee, IrClass,
        IrExpr, IrFile, IrLocalPropertyLayout, SynthesizedAccessorCall,
    };
    use crate::types::{type_name, Ty};

    fn implementation(
        property: PropertyId,
        accessor: BridgeAccessorRole,
    ) -> BridgePropertyImplementation {
        BridgePropertyImplementation { property, accessor }
    }

    fn accessor_bridge(
        kind: BridgeKind,
        property_implementation: Option<BridgePropertyImplementation>,
        name: &str,
    ) -> Bridge {
        Bridge {
            kind,
            target_function: None,
            overridden_owner: None,
            collection_barrier: None,
            parameters: Vec::new(),
            name: name.to_string(),
            erased_params: Vec::new(),
            erased_ret: Ty::obj("kotlin/Any"),
            concrete_params: Vec::new(),
            concrete_ret: Ty::obj("kotlin/Result"),
            target_ret: None,
            barrier_plan: None,
            special: false,
            target_name: None,
            property_implementation,
        }
    }

    fn member(class: u32, property: u32) -> IrLocalPropertyLayout {
        IrLocalPropertyLayout::Member {
            class,
            owner: type_name("sample/Foo"),
            backing_field: Some(0),
            getter: None,
            setter: None,
            interface: false,
            name: "result".to_string(),
            ty: Ty::obj("kotlin/Result"),
            mutable: false,
            private: false,
            context_parameters: Vec::new(),
            property,
        }
    }

    #[test]
    fn a_property_bridge_retargets_only_its_selected_implementation() {
        let selected = PropertyId::from_raw(1);
        let other = PropertyId::from_raw(2);
        let mut ir = IrFile::default();
        let class = ir.add_class(IrClass::synthetic(type_name("sample/Foo")));
        let unlaid = PropertyId::from_raw(3);
        ir.local_property_layouts.insert(selected, member(class, 0));
        ir.local_property_layouts.insert(other, member(class, 1));
        ir.classes[class as usize].bridges.extend([
            accessor_bridge(
                BridgeKind::PropertyGetter,
                Some(implementation(selected, BridgeAccessorRole::Getter)),
                "getResult",
            ),
            accessor_bridge(
                BridgeKind::PropertyGetter,
                Some(implementation(other, BridgeAccessorRole::Getter)),
                "getResult",
            ),
            accessor_bridge(BridgeKind::PropertyGetter, None, "getResult"),
            accessor_bridge(
                BridgeKind::PropertyGetter,
                Some(implementation(unlaid, BridgeAccessorRole::Getter)),
                "getResult",
            ),
            accessor_bridge(
                BridgeKind::PropertySetter,
                Some(implementation(selected, BridgeAccessorRole::Setter)),
                "setResult",
            ),
        ]);
        retarget_property_bridges(
            &mut ir,
            class as usize,
            0,
            "getResult-impl",
            "setResult-impl",
        );
        let bridges = &ir.classes[class as usize].bridges;
        assert_eq!(bridges[0].target_name.as_deref(), Some("getResult-impl"));
        assert_eq!(bridges[1].target_name, None);
        assert_eq!(bridges[2].target_name, None);
        assert_eq!(bridges[3].target_name, None);
        assert_eq!(bridges[4].target_name.as_deref(), Some("setResult-impl"));
    }

    #[test]
    #[should_panic(expected = "does not match accessor")]
    fn a_property_bridge_whose_role_disagrees_with_its_kind_is_not_retargeted_by_spelling() {
        let selected = PropertyId::from_raw(1);
        let mut ir = IrFile::default();
        let class = ir.add_class(IrClass::synthetic(type_name("sample/Foo")));
        ir.local_property_layouts.insert(selected, member(class, 0));
        ir.classes[class as usize].bridges.push(accessor_bridge(
            BridgeKind::PropertyGetter,
            Some(implementation(selected, BridgeAccessorRole::Setter)),
            "getResult",
        ));
        retarget_property_bridges(
            &mut ir,
            class as usize,
            0,
            "getResult-impl",
            "setResult-impl",
        );
    }

    fn virtual_getter(name: &str) -> IrExpr {
        IrExpr::Call {
            callee: Callee::Virtual {
                owner: type_name("sample/Foo"),
                name: name.to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                params: None,
                interface: false,
                module_target: None,
            },
            dispatch_receiver: None,
            args: Vec::new(),
        }
    }

    #[test]
    fn a_bound_virtual_call_takes_the_property_accessor_name() {
        let mut ir = IrFile::default();
        let call = ir.add_expr(virtual_getter("getResult"));
        ir.synthesized_accessor_calls.insert(
            call,
            SynthesizedAccessorCall {
                class: 0,
                property: 0,
            },
        );
        rename_bound_calls(&mut ir, 0, 0, "getResult-impl");
        let IrExpr::Call {
            callee: Callee::Virtual { name, .. },
            ..
        } = &ir.exprs[call as usize]
        else {
            panic!("the bound call stayed virtual");
        };
        assert_eq!(name, "getResult-impl");
    }

    #[test]
    #[should_panic(expected = "does not name an expression")]
    fn a_missing_bound_call_is_not_ignored() {
        let mut ir = IrFile::default();
        ir.synthesized_accessor_calls.insert(
            4,
            SynthesizedAccessorCall {
                class: 0,
                property: 0,
            },
        );
        rename_bound_calls(&mut ir, 0, 0, "getResult-impl");
    }

    #[test]
    #[should_panic(expected = "is not a virtual call")]
    fn a_bound_call_that_is_not_virtual_is_not_left_plain() {
        let mut ir = IrFile::default();
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(0),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        ir.synthesized_accessor_calls.insert(
            call,
            SynthesizedAccessorCall {
                class: 0,
                property: 0,
            },
        );
        rename_bound_calls(&mut ir, 0, 0, "getResult-impl");
    }
}
