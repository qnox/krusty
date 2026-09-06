mod common;

#[test]
fn narrowed_extension_receiver_contributes_its_member_extensions() {
    let source = r#"
        class Bob {
            fun Bob.bar() = "OK"
        }

        fun Any.foo() = when (this) {
            is Bob -> bar()
            else -> throw AssertionError()
        }

        fun box(): String = Bob().foo()
    "#;

    let diagnostics = common::checker_diags_with_stdlib(source)
        .expect("the frontend test toolchain must be available");
    assert_eq!(diagnostics, Vec::<String>::new());
}
