//! The annotations each field of a class states, with their arguments, read again from the class
//! file. The class model keeps only whether a field is annotated, so a cold query does not make
//! every parsed class carry the arguments. A Kotlin enum entry is a static field of the enum, so
//! this is where its annotations live.

use super::{cp_utf8, parse_constant_pool, read_resolved_annotation, ReadError, Reader};
use crate::types::ResolvedAnnotation;

/// The annotations one field states.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldAnnotations {
    pub name: String,
    /// The `RuntimeVisibleAnnotations`, then the `RuntimeInvisibleAnnotations`, each in the order
    /// the class file lists them.
    pub annotations: Vec<ResolvedAnnotation>,
}

/// The annotations each field of the class in `bytes` states, in field order. A field stating none
/// is left out.
pub fn field_annotations(bytes: &[u8]) -> Result<Vec<FieldAnnotations>, ReadError> {
    let mut reader = Reader { b: bytes, i: 0 };
    if reader.u4()? != 0xCAFEBABE {
        return Err(ReadError::NotAClass);
    }
    reader.u2()?;
    reader.u2()?;
    let pool = parse_constant_pool(&mut reader)?;
    reader.u2()?;
    reader.u2()?;
    reader.u2()?;
    let interfaces = reader.u2()?;
    for _ in 0..interfaces {
        reader.u2()?;
    }
    let fields = reader.u2()?;
    let mut out = Vec::new();
    for _ in 0..fields {
        reader.u2()?;
        let name = cp_utf8(&pool, reader.u2()?).ok_or(ReadError::Truncated)?;
        reader.u2()?;
        let mut visible = Vec::new();
        let mut invisible = Vec::new();
        let attributes = reader.u2()?;
        for _ in 0..attributes {
            let attribute = cp_utf8(&pool, reader.u2()?).ok_or(ReadError::Truncated)?;
            let length = reader.u4()? as usize;
            let body = reader.take(length)?;
            let into = match attribute {
                "RuntimeVisibleAnnotations" => &mut visible,
                "RuntimeInvisibleAnnotations" => &mut invisible,
                _ => continue,
            };
            let mut body = Reader { b: body, i: 0 };
            for _ in 0..body.u2()? {
                into.push(read_resolved_annotation(&mut body, &pool)?);
            }
        }
        if !visible.is_empty() || !invisible.is_empty() {
            visible.append(&mut invisible);
            out.push(FieldAnnotations {
                name: name.to_string(),
                annotations: visible,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::ClassWriter;
    use crate::kt_string::KtString;
    use crate::types::{type_name, AnnotationValue};

    fn serial_name(value: &str) -> crate::ir::DeclarationAnnotations {
        crate::ir::DeclarationAnnotations::new(vec![crate::ir::RetainedAnnotation {
            retention: crate::types::AnnotationRetention::Runtime,
            annotation: crate::ir::AppliedAnnotation {
                written_order: None,
                internal: type_name("demo/SerialName"),
                values: vec![(
                    "value".to_string(),
                    crate::ir::AnnoValue::Const(crate::ir::IrConst::String(KtString::from(value))),
                )],
            },
            facts: Default::default(),
        }])
    }

    #[test]
    fn each_annotated_field_states_its_applications_and_the_others_none() {
        let mut writer = ClassWriter::new("demo/Fields", "java/lang/Object");
        writer.add_field(0x0019, "PLAIN", "Ldemo/Fields;");
        writer.add_field(0x0019, "NAMED", "Ldemo/Fields;");
        writer.set_last_field_annotations(&serial_name("wire"));
        writer.add_field(0x0019, "AFTER", "Ldemo/Fields;");

        let read = field_annotations(&writer.finish()).expect("read the fields");
        assert_eq!(
            read,
            vec![FieldAnnotations {
                name: "NAMED".to_string(),
                annotations: vec![ResolvedAnnotation {
                    annotation: type_name("demo/SerialName"),
                    arguments: vec![(
                        "value".to_string(),
                        AnnotationValue::String(KtString::from("wire")),
                    )],
                    facts: Default::default(),
                    written: None,
                }],
            }]
        );
    }
}
