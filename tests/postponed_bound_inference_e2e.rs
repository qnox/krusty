mod common;

#[test]
fn inner_generic_bound_constrains_an_outer_postponed_receiver_type() {
    let source = r#"
        inline fun <R, C : MutableCollection<in R>> collectInto(
            destination: C,
            transform: (List<String>) -> Iterable<R>,
        ) {}

        fun box(): String {
            buildSet {
                collectInto(this) { it }
            }
            return "OK"
        }
    "#;

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(source, &[stdlib], Some(jdk.as_path()));
    assert_eq!(diagnostics, Vec::<String>::new());
}
