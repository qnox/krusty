//! Conversion of frontend-checked declaration annotations into common IR metadata.
//!
//! This is a bounded active-source handoff. Every semantic annotation application was selected and
//! folded by the checker; this module only partitions the retained payload by declaration use site.

use crate::ast;
use crate::resolve::TypeInfo;

fn annotation_value(value: &crate::types::AnnotationValue) -> crate::ir::AnnoValue {
    use crate::types::AnnotationValue;
    match value {
        AnnotationValue::Int(value) => crate::ir::AnnoValue::Const(crate::ir::IrConst::Int(*value)),
        AnnotationValue::Byte(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Byte(*value))
        }
        AnnotationValue::Short(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Short(*value))
        }
        AnnotationValue::Long(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Long(*value))
        }
        AnnotationValue::Float(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Float(*value))
        }
        AnnotationValue::Double(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Double(*value))
        }
        AnnotationValue::Boolean(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Boolean(*value))
        }
        AnnotationValue::Char(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::Char(*value))
        }
        AnnotationValue::String(value) => {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::String(value.clone()))
        }
        AnnotationValue::Enum(internal, constant) => {
            crate::ir::AnnoValue::Enum(*internal, constant.clone())
        }
        AnnotationValue::Class(internal) => crate::ir::AnnoValue::Class(*internal),
        AnnotationValue::Annotation { internal, values } => {
            crate::ir::AnnoValue::Annotation(crate::ir::AppliedAnnotation {
                internal: *internal,
                values: values
                    .iter()
                    .map(|(name, value)| (name.clone(), annotation_value(value)))
                    .collect(),
            })
        }
        AnnotationValue::Array(values) => {
            crate::ir::AnnoValue::Array(values.iter().map(annotation_value).collect())
        }
    }
}

fn applied_annotation(
    annotation: &crate::types::AppliedAnnotation,
) -> crate::ir::AppliedAnnotation {
    crate::ir::AppliedAnnotation {
        internal: annotation.internal,
        values: annotation
            .values
            .iter()
            .map(|(name, value)| (name.clone(), annotation_value(value)))
            .collect(),
    }
}

pub(super) fn declaration_annotations(
    annotations: &[ast::AnnotationRef],
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    crate::ir::DeclarationAnnotations::new(
        annotations
            .iter()
            .map(|annotation| {
                info.applied_annotation(annotation).unwrap_or_else(|| {
                    panic!(
                        "frontend did not record checked annotation application at {}..{}",
                        annotation.span.lo, annotation.span.hi
                    )
                })
            })
            .filter_map(retained_annotation)
            .collect(),
    )
}

fn property_site_annotations(
    declared: &[ast::AnnotationRef],
    info: &TypeInfo,
    on_constructor_parameter: bool,
    site: crate::types::PropertyAnnotationSite,
) -> crate::ir::DeclarationAnnotations {
    crate::ir::DeclarationAnnotations::new(
        declared
            .iter()
            .filter_map(|annotation| info.applied_annotation(annotation))
            .filter(|applied| {
                applied
                    .targets
                    .property_declaration_site(on_constructor_parameter)
                    == Some(site)
            })
            .filter_map(retained_annotation)
            .collect(),
    )
}

fn retained_annotation(
    annotation: &crate::types::AppliedAnnotation,
) -> Option<crate::ir::RetainedAnnotation> {
    match annotation.retention {
        crate::types::AnnotationRetention::Source => None,
        retention => Some(crate::ir::RetainedAnnotation {
            retention,
            annotation: applied_annotation(annotation),
        }),
    }
}

pub(super) fn value_parameter_annotations(
    declared: &[ast::AnnotationRef],
    info: &TypeInfo,
) -> crate::ir::DeclarationAnnotations {
    crate::ir::DeclarationAnnotations::new(
        declared
            .iter()
            .filter_map(|annotation| info.applied_annotation(annotation))
            .filter_map(retained_annotation)
            .collect(),
    )
}

pub(super) fn class_field_annotations(
    class: &ast::ClassDecl,
    info: &TypeInfo,
) -> Vec<crate::ir::FieldAnnotations> {
    let mut out = Vec::new();
    for entry in &class.enum_entries {
        let annotations = declaration_annotations(&entry.annotations, info);
        if !annotations.is_empty() {
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
        .map(|parameter| (&parameter.name, &parameter.annotations, true))
        .chain(
            class
                .body_props
                .iter()
                .map(|property| (&property.name, &property.annotations, false)),
        );
    for (name, declared, on_constructor_parameter) in declarations {
        let annotations = property_site_annotations(
            declared,
            info,
            on_constructor_parameter,
            crate::types::PropertyAnnotationSite::Field,
        );
        if !annotations.is_empty() {
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
) -> Vec<crate::ir::PropertyAnnotations> {
    let mut out = Vec::new();
    let declarations = class
        .props
        .iter()
        .filter(|parameter| parameter.is_property)
        .map(|parameter| (&parameter.name, &parameter.annotations, true))
        .chain(
            class
                .body_props
                .iter()
                .map(|property| (&property.name, &property.annotations, false)),
        );
    for (name, declared, on_constructor_parameter) in declarations {
        let annotations = property_site_annotations(
            declared,
            info,
            on_constructor_parameter,
            crate::types::PropertyAnnotationSite::Property,
        );
        if !annotations.is_empty() {
            out.push(crate::ir::PropertyAnnotations {
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
                    true,
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
    if out.iter().all(crate::ir::DeclarationAnnotations::is_empty) {
        Vec::new()
    } else {
        out
    }
}
