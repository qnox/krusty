//! Realize a Kotlin PROPERTY's own annotations on the JVM.
//!
//! A Kotlin property is not a class-file declaration — the class file has a field and accessor
//! methods, and none of them is the property. kotlinc therefore parks a property-targeted annotation
//! on a synthetic `get<Name>$annotations()V` marker method (`ACC_PUBLIC|ACC_STATIC|ACC_SYNTHETIC`,
//! carrying the classic `Deprecated` attribute so nothing calls it) and names that method from the
//! property's `JvmPropertySignature`, which is how a consumer finds the annotations again.
//!
//! Runs BEFORE the value-class pass, which renames a marker alongside its mangled getter.

use crate::ir::{FnParamInfo, IrExpr, IrFile, IrFunction, IrParameterIdentity};
use crate::names::property_getter_name;
use crate::types::Ty;

/// Add one marker method per property with retained annotations, right after that property's getter
/// (kotlinc's member order), and attach the property's annotations to it. The class keeps its
/// property annotations: `@Metadata` reads the property's own annotation facts from them.
pub fn synthesize_property_annotation_markers(ir: &mut IrFile) {
    for class_index in 0..ir.classes.len() {
        let annotated = ir.classes[class_index].property_annotations.clone();
        for property in annotated {
            // A retained application needs its JVM carrier. A suppressed SOURCE application has
            // no annotation attribute, but kotlinc still emits the empty marker that its property
            // metadata names. An erased optional expectation needs neither.
            if property.annotations.retains_none()
                && !property.annotations.has_suppressed_source_application()
            {
                continue;
            }
            let getter = realized_getter_name(ir, class_index, property.source_order)
                .unwrap_or_else(|| property_getter_name(&property.property));
            let marker_name = format!("{getter}$annotations");
            let class_identity = ir.classes[class_index].fq_name_id();
            let (params, identities, after) =
                ir.property_accessor_parameters(class_index, property.source_order);
            // A plugin may already have synthesized this exact property's marker (the serialization
            // plugin does for `@SerialName`). Consume that recorded declaration edge; a same-named
            // source or unrelated synthetic method is not marker provenance.
            if let Some(&existing) = ir
                .property_annotation_markers
                .get(&(class_identity, property.source_order))
            {
                let function = &ir.functions[existing as usize];
                assert!(
                    ir.synthetic_methods.contains(&existing)
                        && function.name == marker_name
                        && function.params == params
                        && function.ret == Ty::Unit,
                    "a recorded property annotation marker has an invalid JVM declaration"
                );
                ir.function_annotations
                    .insert(existing, property.annotations);
                ir.fn_source_order.insert(existing, property.source_order);
                continue;
            }
            // The getter's position fixes the marker's; a property whose accessor this class does
            // not own still gets its marker, appended last.
            let after = after.and_then(|getter| {
                ir.classes[class_index]
                    .methods
                    .iter()
                    .position(|&fid| fid == getter)
            });
            let marker = synthesize_marker(ir, marker_name, params, identities);
            // Not `final` (kotlinc emits none), not a described declaration (`synthetic_methods` keeps
            // it out of `@Metadata`'s function list — it is named by the PROPERTY's signature instead),
            // and deprecated so a Kotlin consumer never calls it.
            ir.function_annotations.insert(marker, property.annotations);
            ir.fn_source_order.insert(marker, property.source_order);
            ir.property_annotation_markers
                .insert((class_identity, property.source_order), marker);
            let methods = &mut ir.classes[class_index].methods;
            match after {
                Some(index) => methods.insert(index + 1, marker),
                None => methods.push(marker),
            }
        }
    }

    let package_properties = ir
        .package_properties
        .iter()
        .filter(|property| property.suppressed_source_annotation_application)
        .map(|property| (property.property, property.name.clone()))
        .collect::<Vec<_>>();
    for (property, name) in package_properties {
        let getter = realized_package_getter_name(ir, property)
            .unwrap_or_else(|| property_getter_name(&name));
        let marker_name = format!("{getter}$annotations");
        let marker = synthesize_marker(ir, marker_name, Vec::new(), Vec::new());
        ir.mark_jvm_package_property_annotation_marker(property, marker);
    }
}

/// The getter JVM name module realization gave the property declared at `source_order`: a
/// declared getter's function name, or a default getter's written `@JvmName`.
fn realized_getter_name(ir: &IrFile, class_index: usize, source_order: u32) -> Option<String> {
    let property = ir.classes[class_index]
        .properties
        .iter()
        .find(|property| property.source_order == source_order)?;
    match property.getter {
        Some(getter) => Some(ir.functions[getter as usize].name.clone()),
        None => property.getter_jvm_name.clone(),
    }
}

/// The getter JVM name module realization gave a top-level property: a declared getter's function
/// name, or its storage's written `@JvmName`.
fn realized_package_getter_name(ir: &IrFile, property: crate::fir::PropertyId) -> Option<String> {
    match ir.local_property_layouts.get(&property)? {
        crate::ir::IrLocalPropertyLayout::TopLevelStorage {
            storage,
            getter: None,
            ..
        } => ir.statics[*storage as usize].getter_jvm_name.clone(),
        crate::ir::IrLocalPropertyLayout::TopLevelStorage {
            getter: Some(getter),
            ..
        }
        | crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, .. } => {
            Some(ir.functions[*getter as usize].name.clone())
        }
        _ => None,
    }
}

fn synthesize_marker(
    ir: &mut IrFile,
    name: String,
    params: Vec<Ty>,
    identities: Vec<IrParameterIdentity>,
) -> u32 {
    let ret = ir.add_expr(IrExpr::Return(None));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    });
    let marker = ir.add_fun(IrFunction {
        name,
        params,
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.open_methods.insert(marker);
    ir.synthetic_methods.insert(marker);
    ir.deprecated_methods.insert(marker);
    ir.fn_params
        .insert(marker, FnParamInfo::identities(identities));
    marker
}
