mod common;

#[test]
fn explicit_lambda_label_names_its_extension_receiver() {
    let source = r#"
        import kotlin.experimental.ExperimentalTypeInference

        @OptIn(ExperimentalTypeInference::class)
        fun <R> build(block: TestInterface<R>.() -> Unit) {}

        interface TestInterface<R> {
            fun emit(value: R)
        }

        fun box(): String {
            build myLabel@ {
                emit("")
                val receiver = this@myLabel
            }
            return "OK"
        }
    "#;

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(source, &[stdlib], Some(jdk.as_path()));
    assert_eq!(diagnostics, Vec::<String>::new());
}
