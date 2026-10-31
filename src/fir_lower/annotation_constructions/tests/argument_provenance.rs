use super::*;
use crate::metadata::type_encoder::StringTable;
use crate::types::type_name;

#[test]
fn closed_default_replay_preserves_written_order_and_omitted_members() {
    let ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../tests/fixtures/annotation_default_provenance/ClosedDefaults.kt"),
        "ClosedDefaults",
    );
    let inner = type_name("Inner");
    for runtime_omissions in [&[][..], &[0][..]] {
        let payloads = ir
        .annotation_constructions
        .iter()
        .filter(|(expression, construction)| {
            construction.interface == inner
                && matches!(ir.expr(**expression), IrExpr::New { defaults, .. } if defaults.as_ref() == runtime_omissions)
        })
        .map(|(expression, _)| {
            let Some(AnnoValue::Annotation(annotation)) =
                element_value(&ir, *expression, Ty::obj_name(inner))
            else {
                panic!("closed annotation default must remain a complete element value");
            };
            let expected_strings = include_str!(
                    "../../../../tests/fixtures/annotation_default_provenance/strings.txt"
            )
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            let mut strings = StringTable::klib(&|_| Ok(None));
            for string in &expected_strings {
                strings.local(string);
            }
            assert_eq!(strings.class_id(type_name("Outer")), 0);
            assert_eq!(strings.class_id(inner), 1);
            let qualified_names = strings.qualified_name_table().into_bytes();
            let encoded = crate::metadata::builder::annotation_pb(&mut strings, &annotation);
            assert_eq!(strings.qualified_name_table().as_bytes(), qualified_names);
            assert_eq!(strings.into_strings(), expected_strings);
            encoded
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
        .collect::<Vec<_>>();
        assert_eq!(
            payloads,
            [include_str!(
                "../../../../tests/fixtures/annotation_default_provenance/inner.annotation.hex"
            )
            .trim()]
        );
    }
}
