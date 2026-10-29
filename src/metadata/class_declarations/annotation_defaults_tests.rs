use super::*;
use crate::ir::{AnnoValue, AppliedAnnotation, IrField};
use crate::types::type_name;

fn annotation_class() -> IrClass {
    let mut class = crate::plugins::synthetic_class("sample/Annotation");
    class.is_annotation = true;
    class.ctor_param_count = 2;
    let mut defaulted = IrField::new("defaulted".into(), Ty::Int);
    defaulted.flags = defaulted.flags.with_has_default(true);
    class.fields = vec![defaulted, IrField::new("required".into(), Ty::Int)];
    class
}

#[test]
fn missing_normalized_defaults_remain_visible_at_the_emission_boundary() {
    let class = annotation_class();
    let mut ir = IrFile::default();
    let record = build(&ir, &class).expect("annotation declaration");
    assert_eq!(record.missing_annotation_default(), Some("defaulted"));
    assert_eq!(record.annotation_param_defaults.as_ref().unwrap().len(), 2);

    ir.annotation_element_defaults.insert(
        class.fq_name_id(),
        vec![(
            "defaulted".into(),
            AnnoValue::Const(crate::ir::IrConst::Int(3)),
        )],
    );
    let record = build(&ir, &class).expect("annotation declaration");
    assert_eq!(record.missing_annotation_default(), None);
    let mut ordinary = class.clone();
    ordinary.is_annotation = false;
    let record = build(&ir, &ordinary).expect("ordinary declaration");
    assert!(record.annotation_param_defaults.is_none());
    assert_eq!(record.missing_annotation_default(), None);
}

#[test]
fn recorded_defaults_follow_recursive_classifier_remapping() {
    let class = annotation_class();
    let before = type_name("sample/Before");
    let after = type_name("sample/After");
    let value = |classifier| {
        AnnoValue::Array(vec![
            AnnoValue::Class(Ty::obj_name(classifier)),
            AnnoValue::Enum(classifier, "ENTRY".into()),
            AnnoValue::Annotation(AppliedAnnotation {
                internal: classifier,
                values: vec![("type".into(), AnnoValue::Class(Ty::obj_name(classifier)))],
            }),
        ])
    };
    let mut ir = IrFile::default();
    ir.annotation_element_defaults.insert(
        class.fq_name_id(),
        vec![("defaulted".into(), value(before))],
    );
    ir.add_class(class);
    record_all(&mut ir);
    ir.remap_classifier_identities(&crate::fx_hash::HashMap::from_iter([(before, after)]));
    let record = ir.class_declarations[&type_name("sample/Annotation")]
        .as_ref()
        .expect("annotation declaration");
    assert_eq!(
        format!("{:?}", record.annotation_param_defaults),
        format!("{:?}", Some(vec![Some(value(after)), None]))
    );
}
