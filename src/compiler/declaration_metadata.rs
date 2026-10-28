//! Conversion of frontend-checked declaration annotations into common IR metadata.
//!
//! This is a bounded active-source handoff. Every semantic annotation application was selected and
//! folded by the checker; this module only partitions the retained payload by declaration use site.

use crate::ast;
use crate::resolve::TypeInfo;

fn applied_annotation(
    annotation: &crate::types::AppliedAnnotation,
) -> crate::ir::AppliedAnnotation {
    crate::ir::AppliedAnnotation {
        internal: annotation.internal,
        values: annotation
            .values
            .iter()
            .map(|(name, value)| (name.clone(), crate::ir::AnnoValue::from(value)))
            .collect(),
    }
}

pub(super) fn declaration_annotations(
    annotations: &[ast::AnnotationRef],
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    checked_annotations(annotations.iter().filter_map(|annotation| {
        match info.applied_annotation(annotation) {
            Some(applied) => Some(applied),
            None if info.discarded_annotation_application(annotation) => None,
            None => panic!(
                "frontend did not record checked annotation application at {}..{}",
                annotation.span.lo, annotation.span.hi
            ),
        }
    }))
}

/// The classifiers of the SOURCE-retained annotations `annotations` apply, in source order. No class
/// file or metadata record names them; a compiler plugin may still read them.
pub(super) fn source_retained_annotations(
    annotations: &[ast::AnnotationRef],
    info: &TypeInfo,
) -> Vec<crate::types::TypeName> {
    annotations
        .iter()
        .filter_map(|annotation| info.applied_annotation(annotation))
        .filter(|applied| applied.retention == crate::types::AnnotationRetention::Source)
        .map(|applied| applied.internal)
        .collect()
}

fn property_site_annotations(
    declared: &[ast::AnnotationRef],
    info: &TypeInfo,
    site: crate::types::PropertyAnnotationSite,
) -> crate::ir::DeclarationAnnotations {
    // Kotlin lowers explicit use-site applications before annotations whose declaration target
    // defaults to that site, even when the defaulted annotation was written first. Preserve source
    // order inside each group; this is the annotation order emitted on the physical declaration.
    let applications = [true, false].into_iter().flat_map(|explicit| {
        declared.iter().filter_map(move |annotation| {
            if annotation.property_site.is_some() != explicit {
                return None;
            }
            let applied = info.applied_annotation(annotation)?;
            applied
                .property_sites
                .expect("frontend recorded the property application's selected destinations")
                .contains(site)
                .then_some(applied)
        })
    });
    let suppressed_source_application = site == crate::types::PropertyAnnotationSite::Property
        && declared
            .iter()
            .any(|annotation| info.retains_suppressed_source_property_application(annotation));
    checked_annotations(applications)
        .with_suppressed_source_application(suppressed_source_application)
}

fn checked_annotations<'a>(
    applications: impl Iterator<Item = &'a crate::types::AppliedAnnotation>,
) -> crate::ir::DeclarationAnnotations {
    crate::ir::DeclarationAnnotations::from_checked(
        applications.map(|checked| (checked, applied_annotation(checked))),
    )
}

/// The checked annotations written on `property`'s accessors and on its setter's value parameter.
pub(super) fn accessor_annotations(
    property: &ast::PropDecl,
    info: &TypeInfo,
) -> crate::ir::AccessorAnnotations {
    let setter = property.setter.as_ref();
    crate::ir::AccessorAnnotations {
        getter: accessor_site_annotations(
            &property.annotations,
            &property.getter_annotations,
            crate::types::PropertyAnnotationSite::Getter,
            property.getter.is_some(),
            info,
        ),
        setter: accessor_site_annotations(
            &property.annotations,
            setter.map_or(&[], |setter| setter.annotations.as_slice()),
            crate::types::PropertyAnnotationSite::Setter,
            setter.is_some_and(|setter| setter.body.is_some()),
            info,
        ),
        setter_parameter: if setter.is_some_and(|setter| setter.param.is_none()) {
            Default::default()
        } else {
            accessor_site_annotations(
                &property.annotations,
                setter.map_or(&[], |setter| setter.param_annotations.as_slice()),
                crate::types::PropertyAnnotationSite::SetterParameter,
                setter.is_some_and(|setter| setter.body.is_some()),
                info,
            )
        },
    }
}

/// The checked annotations a primary-constructor property sends to its generated accessors.
pub(super) fn constructor_property_accessor_annotations(
    property: &ast::PropParam,
    info: &TypeInfo,
) -> crate::ir::AccessorAnnotations {
    let site = |target| accessor_site_annotations(&property.annotations, &[], target, false, info);
    crate::ir::AccessorAnnotations {
        getter: site(crate::types::PropertyAnnotationSite::Getter),
        setter: site(crate::types::PropertyAnnotationSite::Setter),
        setter_parameter: site(crate::types::PropertyAnnotationSite::SetterParameter),
    }
}

/// Merge an accessor's own annotations with the property-prefix applications targeted at it.
/// kotlinc visits a targeted application first for an accessor with a body and after a written
/// bodyless accessor.
fn accessor_site_annotations(
    property_annotations: &[ast::AnnotationRef],
    accessor_annotations: &[ast::AccessorAnnotation],
    target: crate::types::PropertyAnnotationSite,
    has_body: bool,
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    let targeted = property_annotations
        .iter()
        .filter(|annotation| annotation.property_site == Some(target))
        .filter_map(|annotation| info.applied_annotation(annotation));
    let declared = accessor_annotations
        .iter()
        .filter_map(|entry| info.applied_annotation(&entry.annotation));
    if has_body {
        checked_annotations(targeted.chain(declared))
    } else {
        checked_annotations(declared.chain(targeted))
    }
}

pub(super) fn top_level_property_retains_suppressed_source_application(
    property: &ast::PropDecl,
    info: &TypeInfo,
) -> bool {
    property
        .annotations
        .iter()
        .any(|annotation| info.retains_suppressed_source_property_application(annotation))
}

/// The PROPERTY-targeted applications declared on a separately hoisted `companion { ... }`
/// property. The property is not part of its classifier's ordinary AST property lists, so its
/// checked applications cross the metadata handoff with that declaration itself.
pub(super) fn companion_block_property_annotations(
    property: &ast::PropDecl,
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    property_site_annotations(
        &property.annotations,
        info,
        crate::types::PropertyAnnotationSite::Property,
    )
}

/// The FIELD-targeted applications declared on a separately hoisted `companion { ... }`
/// property. They belong to the block property's exact static storage declaration, not to an
/// ordinary same-named field of the classifier.
pub(super) fn companion_block_field_annotations(
    property: &ast::PropDecl,
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    property_site_annotations(
        &property.annotations,
        info,
        crate::types::PropertyAnnotationSite::Field,
    )
}

pub(super) fn value_parameter_annotations(
    declared: &[ast::AnnotationRef],
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    checked_annotations(
        declared
            .iter()
            .filter_map(|annotation| info.applied_annotation(annotation)),
    )
}

pub(super) fn class_field_annotations(
    class: &ast::ClassDecl,
    info: &TypeInfo,
) -> Vec<crate::ir::FieldAnnotations> {
    let mut out = Vec::new();
    for entry in &class.enum_entries {
        let annotations = declaration_annotations(&entry.annotations, info);
        if annotations.declares_annotations() {
            out.push(crate::ir::FieldAnnotations {
                field: entry.name.clone(),
                annotations,
            });
        }
    }
    let declarations = class
        .props
        .iter()
        .filter(|parameter| parameter.is_property)
        .map(|parameter| (&parameter.name, &parameter.annotations))
        .chain(
            class
                .body_props
                .iter()
                .map(|property| (&property.name, &property.annotations)),
        );
    for (name, declared) in declarations {
        let annotations =
            property_site_annotations(declared, info, crate::types::PropertyAnnotationSite::Field);
        if annotations.declares_annotations() {
            out.push(crate::ir::FieldAnnotations {
                field: name.clone(),
                annotations,
            });
        }
    }
    out
}

pub(super) fn class_property_annotations(
    class: &ast::ClassDecl,
    info: &TypeInfo,
    source_orders: &[Option<u32>],
) -> Vec<crate::ir::PropertyAnnotations> {
    let mut out = Vec::new();
    let declaration_count = class
        .props
        .iter()
        .filter(|parameter| parameter.is_property)
        .count()
        + class.body_props.len();
    assert_eq!(
        declaration_count,
        source_orders.len(),
        "every class property has one stable declaration identity slot"
    );
    let declarations = class
        .props
        .iter()
        .filter(|parameter| parameter.is_property)
        .map(|parameter| (&parameter.name, &parameter.annotations))
        .chain(
            class
                .body_props
                .iter()
                .map(|property| (&property.name, &property.annotations)),
        );
    for ((name, declared), &source_order) in declarations.zip(source_orders) {
        let annotations = property_site_annotations(
            declared,
            info,
            crate::types::PropertyAnnotationSite::Property,
        );
        if let Some(source_order) = source_order.filter(|_| annotations.declares_annotations()) {
            out.push(crate::ir::PropertyAnnotations {
                source_order,
                property: name.clone(),
                annotations,
            });
        }
    }
    out
}

pub(super) fn primary_constructor_parameter_annotations(
    class: &ast::ClassDecl,
    info: &TypeInfo,
    leading: usize,
) -> Vec<crate::ir::DeclarationAnnotations> {
    let mut out = class
        .props
        .iter()
        .map(|parameter| {
            if parameter.is_property {
                property_site_annotations(
                    &parameter.annotations,
                    info,
                    crate::types::PropertyAnnotationSite::ValueParameter,
                )
            } else {
                value_parameter_annotations(&parameter.annotations, info)
            }
        })
        .collect::<Vec<_>>();
    for _ in 0..leading {
        out.insert(0, crate::ir::DeclarationAnnotations::default());
    }
    if !out
        .iter()
        .any(crate::ir::DeclarationAnnotations::declares_annotations)
    {
        Vec::new()
    } else {
        out
    }
}
