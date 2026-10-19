use super::common;

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

#[test]
fn an_unresolved_non_reified_builder_argument_does_not_publish_a_fir_substitution() {
    let source = r#"
        class First<T> {
            fun <U> first(configure: Second<U>.() -> Unit) {}
        }

        class Second<T> {
            fun <U> second(configure: Third<T, U>.() -> Unit) {}
        }

        class Third<T, U> {
            fun resolve(value: U) {}
        }

        fun box(): String {
            First<String>().first<String> {
                second { resolve("") }
            }
            return "OK"
        }
    "#;

    common::assert_accepted_like_kotlinc(source);
}

#[test]
fn a_contextually_adapted_inner_constructor_does_not_publish_a_partial_substitution() {
    let source = r#"
        class Outer<T> {
            inner class Inner<U>
        }

        fun <T> accept(factory: Outer<T>.() -> Outer<T>.Inner<T>) {
            val value: Outer<T>.Inner<T> = factory(Outer())
        }

        fun box(): String {
            accept(Outer<Int>::Inner)
            return "OK"
        }
    "#;

    common::assert_accepted_like_kotlinc(source);
}
