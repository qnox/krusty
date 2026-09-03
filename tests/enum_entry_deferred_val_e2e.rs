mod common;

#[test]
fn enum_entry_init_may_assign_a_deferred_val_after_a_local_function() {
    let source = r#"
        enum class X {
            B {
                val value2 = "K"
                val value3: String

                init {
                    fun foo() = value2
                    value3 = "O" + foo()
                }

                override val value = value3
            };

            abstract val value: String
        }

        fun box(): String = X.B.value
    "#;

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(source, &[stdlib], Some(jdk.as_path()));
    assert_eq!(diagnostics, Vec::<String>::new());
}
