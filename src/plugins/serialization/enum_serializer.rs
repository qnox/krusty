//! Construction of the runtime serializer factory call for an enum declaration.

use super::{
    class_ty, type_name, Callee, ClassId, ExprId, InlineKind, IrConst, IrExpr, IrFile, KtString, Ty,
};

const SERIAL_NAME_FQ: &str = "kotlinx/serialization/SerialName";
const ANNOTATED_ENUM_SERIALIZER_DESCRIPTOR: &str = "(Ljava/lang/String;[Ljava/lang/Enum;[Ljava/lang/String;[[Ljava/lang/annotation/Annotation;[Ljava/lang/annotation/Annotation;)Lkotlinx/serialization/KSerializer;";

/// Build the factory call captured by an enum's cached-serializer initializer.
pub(super) fn factory_call(
    ir: &mut IrFile,
    class_id: ClassId,
    name: ExprId,
    enums: ExprId,
) -> ExprId {
    // An entry's `@SerialName` is the name the format reads and writes for that constant. It lives
    // on the entry's static FIELD (an enum entry has no property of its own). Compare the resolved
    // annotation identity: an unrelated same-simple-name annotation cannot change the wire format.
    let serial_name_annotation = type_name(SERIAL_NAME_FQ);
    let entry_serial_names: Vec<Option<KtString>> = {
        let class = &ir.classes[class_id as usize];
        class
            .enum_entries
            .iter()
            .map(|entry| {
                class
                    .field_annotations
                    .iter()
                    .find(|annotations| annotations.field == entry.name)
                    .and_then(|annotations| {
                        annotations
                            .annotations
                            .applications()
                            .find(|applied| applied.internal == serial_name_annotation)
                    })
                    .and_then(|applied| {
                        applied
                            .values
                            .iter()
                            .find(|(parameter, _)| parameter == "value")
                    })
                    .and_then(|(_, value)| match value {
                        crate::ir::AnnoValue::Const(IrConst::String(name)) => Some(name.clone()),
                        _ => None,
                    })
            })
            .collect()
    };

    // `createSimpleEnumSerializer` derives every name from the constant spelling. A serial name
    // selects `createAnnotatedEnumSerializer`, whose null name elements mean “use that spelling”.
    if entry_serial_names.iter().all(Option::is_none) {
        return ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlinx/serialization/internal/EnumsKt"),
                name: "createSimpleEnumSerializer".to_string(),
                descriptor:
                    "(Ljava/lang/String;[Ljava/lang/Enum;)Lkotlinx/serialization/KSerializer;"
                        .to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![name, enums],
        });
    }

    let names: Vec<ExprId> = entry_serial_names
        .iter()
        .map(|name| {
            ir.add_expr(IrExpr::Const(match name {
                Some(name) => IrConst::String(name.clone()),
                None => IrConst::Null,
            }))
        })
        .collect();
    let names_array = ir.add_expr(IrExpr::Vararg {
        array_type: Ty::obj_args("kotlin/Array", &[Ty::nullable(class_ty("kotlin/String"))]),
        spreads: vec![false; names.len()],
        elements: names,
    });
    // Other entry/class annotations are a separate retained-annotation materialization gap. Keep
    // their array slots explicit rather than fabricating values from source spellings.
    let entry_annotations: Vec<ExprId> = entry_serial_names
        .iter()
        .map(|_| ir.add_expr(IrExpr::Const(IrConst::Null)))
        .collect();
    let entry_annotations_array = ir.add_expr(IrExpr::Vararg {
        array_type: Ty::obj_args(
            "kotlin/Array",
            &[Ty::nullable(Ty::obj_args(
                "kotlin/Array",
                &[class_ty("kotlin/Annotation")],
            ))],
        ),
        spreads: vec![false; entry_annotations.len()],
        elements: entry_annotations,
    });
    let class_annotations = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: type_name("kotlinx/serialization/internal/EnumsKt"),
            name: "createAnnotatedEnumSerializer".to_string(),
            descriptor: ANNOTATED_ENUM_SERIALIZER_DESCRIPTOR.to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![
            name,
            enums,
            names_array,
            entry_annotations_array,
            class_annotations,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::{
        factory_call, type_name, Callee, IrConst, IrExpr, IrFile, KtString, SERIAL_NAME_FQ,
    };
    use crate::plugins::synthetic_class;

    fn retained_serial_name(internal: &str) -> crate::ir::FieldAnnotations {
        crate::ir::FieldAnnotations {
            field: "ENTRY".to_string(),
            annotations: crate::ir::DeclarationAnnotations::new(vec![
                crate::ir::RetainedAnnotation {
                    retention: crate::ir::AnnoRetention::Runtime,
                    annotation: crate::ir::AppliedAnnotation {
                        internal: type_name(internal),
                        values: vec![
                            (
                                "other".to_string(),
                                crate::ir::AnnoValue::Const(IrConst::String(KtString::from(
                                    "ignored",
                                ))),
                            ),
                            (
                                "value".to_string(),
                                crate::ir::AnnoValue::Const(IrConst::String(KtString::from(
                                    "wire",
                                ))),
                            ),
                        ],
                    },
                },
            ]),
        }
    }

    fn entry() -> crate::ir::IrEnumEntry {
        crate::ir::IrEnumEntry {
            name: "ENTRY".to_string(),
            argument_prelude: Vec::new(),
            args: Vec::new(),
            constructor_parameter_types: Vec::new(),
            default_parameters: Vec::new(),
            decl_line: 0,
            subclass: None,
        }
    }

    #[test]
    fn serial_name_selection_uses_the_resolved_annotation_and_named_argument() {
        let mut ir = IrFile::default();
        let mut class = synthetic_class("Status");
        class.enum_entries = vec![entry()];
        class.field_annotations = vec![retained_serial_name("example/SerialName")];
        let class = ir.add_class(class);

        let name_expr = ir.add_expr(IrExpr::Const(IrConst::Null));
        let enums = ir.add_expr(IrExpr::Const(IrConst::Null));
        let simple = factory_call(&mut ir, class, name_expr, enums);
        let IrExpr::Call {
            callee: Callee::Static {
                name: callee_name, ..
            },
            ..
        } = ir.expr(simple)
        else {
            panic!("enum serializer factory must be a static call")
        };
        assert_eq!(callee_name, "createSimpleEnumSerializer");

        ir.classes[class as usize].field_annotations = vec![retained_serial_name(SERIAL_NAME_FQ)];
        let annotated = factory_call(&mut ir, class, name_expr, enums);
        let IrExpr::Call {
            callee: Callee::Static {
                name: callee_name, ..
            },
            args,
            ..
        } = ir.expr(annotated)
        else {
            panic!("enum serializer factory must be a static call")
        };
        assert_eq!(callee_name, "createAnnotatedEnumSerializer");
        let IrExpr::Vararg { elements, .. } = ir.expr(args[2]) else {
            panic!("annotated enum names must be an array")
        };
        assert!(matches!(
            ir.expr(elements[0]),
            IrExpr::Const(IrConst::String(value)) if value.as_str() == Some("wire")
        ));
    }
}
