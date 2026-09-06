mod common;

#[test]
fn nested_class_const_folds_inside_annotation_string_template() {
    const SOURCE: &str = r#"
        annotation class Ann(val value: String)

        class Container {
            object Nested {
                const val VALUE = "value"
            }
        }

        @Ann("${Container.Nested.VALUE}+")
        fun box(): String = "OK"
    "#;

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(SOURCE, &[stdlib], Some(jdk.as_path()));
    assert_eq!(diagnostics, Vec::<String>::new());
}
