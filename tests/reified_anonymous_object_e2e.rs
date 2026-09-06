mod common;

#[test]
fn inline_anonymous_object_preserves_its_enclosing_reified_parameter() {
    let source = r#"
        interface I
        class C<T>

        private inline fun <reified T> C<T>.f() = object : I {
            val unused = T::class
        }

        fun box(): String {
            val first = C<String>().f()
            val second = C<String>().f()
            arrayOf(first, second)
            return "OK"
        }
    "#;

    let diagnostics = common::checker_diags_with_stdlib(source)
        .expect("the frontend test toolchain must be available");
    assert_eq!(diagnostics, Vec::<String>::new());
}
