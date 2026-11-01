use super::common;

#[test]
fn an_alias_and_a_nested_class_can_share_a_flattened_jvm_name() {
    const SOURCE: &str = r#"
        class Owner { class Member { fun value(): String = "class" } }
        typealias `Owner$Member` = String
        fun fromAlias(value: `Owner$Member`): String = value
        fun fromClass(value: Owner.Member): String = value.value()
        fun box(): String = if (fromAlias("alias") == "alias" && fromClass(Owner.Member()) == "class") "OK" else "fail"
    "#;
    common::expect_box_same_as_kotlinc(SOURCE, "ClassAndAliasDeclarationIdentity");
}
