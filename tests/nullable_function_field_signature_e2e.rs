use super::common;

#[test]
fn nullable_function_fields_retain_their_complete_generic_signatures() {
    let source = include_str!("box_regressions/nullableFunctionFields.kt");
    let classes = common::classes_against_kotlinc_module(&[("nullableFunctionFields.kt", source)]);
    let fields = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        let bytes = classes
            .get("NullableFunctionFields")
            .expect("declared class");
        krusty::jvm::classreader::parse_class(bytes)
            .expect("readable class")
            .fields
            .into_iter()
            .map(|field| (field.access, field.name, field.descriptor, field.signature))
            .collect::<Vec<_>>()
    };
    assert_eq!(fields(&classes.krusty), fields(&classes.reference));
}
