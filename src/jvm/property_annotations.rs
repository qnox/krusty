//! Realize a Kotlin PROPERTY's own annotations on the JVM.
//!
//! A Kotlin property is not a class-file declaration — the class file has a field and accessor
//! methods, and none of them is the property. kotlinc therefore parks a property-targeted annotation
//! on a synthetic `get<Name>$annotations()V` marker method (`ACC_PUBLIC|ACC_STATIC|ACC_SYNTHETIC`,
//! carrying the classic `Deprecated` attribute so nothing calls it) and names that method from the
//! property's `JvmPropertySignature`, which is how a consumer finds the annotations again.
//!
//! Runs BEFORE the value-class pass, which renames a marker alongside its mangled getter.

use crate::ir::{IrExpr, IrFile, IrFunction};
use crate::names::property_getter_name;
use crate::types::Ty;

/// Add one marker method per annotated property, right after that property's getter (kotlinc's
/// member order), and attach the property's annotations to it.
pub fn synthesize_property_annotation_markers(ir: &mut IrFile) {
    for class_index in 0..ir.classes.len() {
        let annotated = std::mem::take(&mut ir.classes[class_index].property_annotations);
        for property in annotated {
            let getter = property_getter_name(&property.property);
            let marker_name = format!("{getter}$annotations");
            // A plugin may already have synthesized this property's marker (the serialization plugin
            // does for `@SerialName`). Attach the annotations to THAT method rather than emitting a
            // second one with the same name and descriptor, which would not even load.
            if let Some(&existing) = ir.classes[class_index]
                .methods
                .iter()
                .find(|&&fid| ir.functions[fid as usize].name == marker_name)
            {
                ir.function_annotations.insert(
                    existing,
                    crate::ir::FnAnnotations {
                        visible: property.visible,
                        invisible: property.invisible,
                    },
                );
                let class_identity = ir.classes[class_index].fq_name_id();
                ir.property_annotation_markers
                    .insert((class_identity, property.property.clone()), existing);
                continue;
            }
            // The getter's position fixes the marker's; a property whose accessor this class does
            // not own still gets its marker, appended last.
            let after = ir.classes[class_index]
                .methods
                .iter()
                .position(|&fid| ir.functions[fid as usize].name == getter);
            let ret = ir.add_expr(IrExpr::Return(None));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            let marker = ir.add_fun(IrFunction {
                name: marker_name,
                params: vec![],
                ret: Ty::Unit,
                body: Some(body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
            // Not `final` (kotlinc emits none), not a described declaration (`synthetic_methods` keeps
            // it out of `@Metadata`'s function list — it is named by the PROPERTY's signature instead),
            // and deprecated so a Kotlin consumer never calls it.
            ir.open_methods.insert(marker);
            ir.synthetic_methods.insert(marker);
            ir.deprecated_methods.insert(marker);
            ir.function_annotations.insert(
                marker,
                crate::ir::FnAnnotations {
                    visible: property.visible,
                    invisible: property.invisible,
                },
            );
            let class_identity = ir.classes[class_index].fq_name_id();
            ir.property_annotation_markers
                .insert((class_identity, property.property.clone()), marker);
            let methods = &mut ir.classes[class_index].methods;
            match after {
                Some(index) => methods.insert(index + 1, marker),
                None => methods.push(marker),
            }
        }
    }
}
