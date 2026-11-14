//! The annotations each method of a class states, with their arguments, read again from the class
//! file for a consumer that shows declarations. The class model keeps only the classes of a
//! method's annotations, so a cold query does not make every parsed class carry the arguments.

use super::{
    cp_utf8, parse_constant_pool, read_resolved_annotation, skip_member_table, ReadError, Reader,
};
use crate::types::ResolvedAnnotation;

/// The annotations one method states.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodAnnotations {
    pub name: String,
    pub descriptor: String,
    /// The `RuntimeVisibleAnnotations`, then the `RuntimeInvisibleAnnotations`, each in the order
    /// the class file lists them.
    pub annotations: Vec<ResolvedAnnotation>,
}

/// The annotations each method of the class in `bytes` states, in method order. A method stating
/// none is left out.
pub fn method_annotations(bytes: &[u8]) -> Result<Vec<MethodAnnotations>, ReadError> {
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
    skip_member_table(&mut reader)?;
    let methods = reader.u2()?;
    let mut out = Vec::new();
    for _ in 0..methods {
        reader.u2()?;
        let name = cp_utf8(&pool, reader.u2()?).ok_or(ReadError::Truncated)?;
        let descriptor = cp_utf8(&pool, reader.u2()?).ok_or(ReadError::Truncated)?;
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
            out.push(MethodAnnotations {
                name: name.to_string(),
                descriptor: descriptor.to_string(),
                annotations: visible,
            });
        }
    }
    Ok(out)
}

/// The applications of one exact method, keeping the physical attribute's retention partition.
pub fn read_method_annotation_applications(
    bytes: &[u8],
    name: &str,
    descriptor: &str,
) -> Option<Vec<(crate::types::AnnotationRetention, ResolvedAnnotation)>> {
    let mut r = Reader { b: bytes, i: 0 };
    if r.u4().ok()? != 0xCAFEBABE {
        return None;
    }
    r.u2().ok()?; // minor
    r.u2().ok()?; // major
    let cp = parse_constant_pool(&mut r).ok()?;
    let utf8 = |index: u16| match cp.get(index as usize) {
        Some(super::C::Utf8(value)) => value.as_str(),
        _ => "",
    };
    r.u2().ok()?; // access_flags
    r.u2().ok()?; // this_class
    r.u2().ok()?; // super_class
    let interfaces = r.u2().ok()?;
    for _ in 0..interfaces {
        r.u2().ok()?;
    }
    for _ in 0..r.u2().ok()? {
        r.u2().ok()?; // access
        r.u2().ok()?; // name
        r.u2().ok()?; // descriptor
        super::skip_attributes(&mut r).ok()?;
    }
    for _ in 0..r.u2().ok()? {
        r.u2().ok()?; // access
        let method_name = utf8(r.u2().ok()?);
        let method_descriptor = utf8(r.u2().ok()?);
        if method_name != name || method_descriptor != descriptor {
            super::skip_attributes(&mut r).ok()?;
            continue;
        }
        let mut annotations = Vec::new();
        for _ in 0..r.u2().ok()? {
            let attribute = utf8(r.u2().ok()?);
            let length = usize::try_from(r.u4().ok()?).ok()?;
            let body = r.take(length).ok()?;
            let retention = match attribute {
                "RuntimeVisibleAnnotations" => crate::types::AnnotationRetention::Runtime,
                "RuntimeInvisibleAnnotations" => crate::types::AnnotationRetention::Binary,
                _ => continue,
            };
            let mut applications = Vec::new();
            super::read_class_annotations(body, &cp, &mut applications).ok()?;
            annotations.extend(
                applications
                    .into_iter()
                    .map(|application| (retention, application)),
            );
        }
        return Some(annotations);
    }
    None
}

/// The applications of one exact method without their physical retention partition.
pub fn read_method_annotations(
    bytes: &[u8],
    name: &str,
    descriptor: &str,
) -> Option<Vec<ResolvedAnnotation>> {
    read_method_annotation_applications(bytes, name, descriptor).map(|applications| {
        applications
            .into_iter()
            .map(|(_, application)| application)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use crate::ir::{
        AnnoRetention, AnnoValue, AppliedAnnotation, DeclarationAnnotations, IrConst,
        RetainedAnnotation,
    };
    use crate::types::{type_name, AnnotationValue, ResolvedAnnotation};

    fn retained(
        retention: AnnoRetention,
        class: &str,
        values: Vec<(String, AnnoValue)>,
    ) -> RetainedAnnotation {
        RetainedAnnotation {
            retention,
            annotation: AppliedAnnotation {
                written_order: None,
                internal: type_name(class),
                values,
            },
            facts: Default::default(),
        }
    }

    #[test]
    fn a_method_states_its_visible_then_its_invisible_annotations_with_their_arguments() {
        let mut writer = crate::jvm::classfile::ClassWriter::new("sample/Use", "java/lang/Object");
        writer.add_abstract_method(0x0401, "value", "()Ljava/lang/String;");
        writer.add_abstract_method(0x0401, "plain", "()I");
        writer.set_method_annotations(
            "value",
            "()Ljava/lang/String;",
            &DeclarationAnnotations::new(vec![
                retained(AnnoRetention::Binary, "sample/Note", Vec::new()),
                retained(
                    AnnoRetention::Runtime,
                    "sample/Alias",
                    vec![(
                        "member".to_string(),
                        AnnoValue::Const(IrConst::String("prefix".into())),
                    )],
                ),
            ]),
        );
        let bytes = writer.finish();
        let read = super::method_annotations(&bytes).unwrap();
        let retained =
            super::read_method_annotation_applications(&bytes, "value", "()Ljava/lang/String;")
                .expect("the selected method has readable attributes");
        assert_eq!(
            retained
                .iter()
                .map(|(retention, _)| *retention)
                .collect::<Vec<_>>(),
            [AnnoRetention::Runtime, AnnoRetention::Binary]
        );
        assert_eq!(
            retained
                .into_iter()
                .map(|(_, application)| application)
                .collect::<Vec<_>>(),
            read[0].annotations
        );
        assert_eq!(
            super::read_method_annotation_applications(&bytes, "plain", "()I"),
            Some(Vec::new())
        );
        assert_eq!(
            super::read_method_annotation_applications(&bytes, "missing", "()I"),
            None
        );
        let annotation =
            |class: &str, arguments: Vec<(String, AnnotationValue)>| ResolvedAnnotation {
                annotation: type_name(class),
                arguments,
                facts: Default::default(),
                written: None,
            };
        assert_eq!(
            read,
            vec![super::MethodAnnotations {
                name: "value".to_string(),
                descriptor: "()Ljava/lang/String;".to_string(),
                annotations: vec![
                    annotation(
                        "sample/Alias",
                        vec![(
                            "member".to_string(),
                            AnnotationValue::String("prefix".into())
                        )]
                    ),
                    annotation("sample/Note", Vec::new()),
                ],
            }]
        );
    }
}
