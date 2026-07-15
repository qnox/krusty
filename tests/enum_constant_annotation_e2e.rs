//! Enum constants may carry annotations (`@SerialName("system") SYSTEM(...)` — pervasive in
//! kotlinx.serialization enums). krusty's enum-entry parser stopped at the leading `@` and reported
//! "unsupported enum member"; it now parses-and-discards the annotation and emits the enum.

use super::common;

fn class_names(src: &str) -> Vec<String> {
    let classes = common::compile_in_process(src, "File", &[], None)
        .unwrap_or_else(|| panic!("krusty failed to compile:\n{src}"));
    classes.into_iter().map(|(name, _)| name).collect()
}

#[test]
fn annotated_enum_constants_compile() {
    let names = class_names(
        "package demo\n\
         annotation class SerialName(val value: String)\n\
         enum class Role(val value: String) {\n\
             @SerialName(\"system\") SYSTEM(\"system\"),\n\
             @SerialName(\"user\") USER(\"user\"),\n\
             ;\n\
         }\n",
    );
    assert!(
        names.contains(&"demo/Role".to_string()),
        "classes: {names:?}"
    );
}

#[test]
fn annotated_enum_constants_without_trailing_semicolon() {
    let names = class_names(
        "package demo\n\
         annotation class SerialName(val value: String)\n\
         enum class Status {\n\
             @SerialName(\"ok\") OK,\n\
             @SerialName(\"fail\") FAIL,\n\
         }\n",
    );
    assert!(
        names.contains(&"demo/Status".to_string()),
        "classes: {names:?}"
    );
}
