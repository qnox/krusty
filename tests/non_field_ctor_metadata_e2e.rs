use super::common;
use krusty::jvm::classreader::parse_class;
use krusty::jvm::metadata::class_constructor_params;

#[test]
fn non_field_constructor_parameter_is_recorded() {
    const SRC: &str = "open class Base(val value: String)\n\
        class Child(value: String = \"x\") : Base(value)\n";
    let classes =
        common::compile_in_process_metadata_cp(SRC, "Child", &[]).expect("compile metadata");
    let child = classes
        .iter()
        .find_map(|(name, bytes)| {
            (name == "Child").then(|| parse_class(bytes).expect("parse Child"))
        })
        .expect("Child class");
    let constructors = class_constructor_params(&child);

    assert_eq!(constructors.len(), 1);
    assert_eq!(constructors[0].names, ["value"]);
    assert_eq!(constructors[0].defaults, [true]);
}
