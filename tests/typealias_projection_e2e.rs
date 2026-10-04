//! Variance written on a typealias parameter survives use-site substitution.
//!
//! `typealias T3<X, Y> = MutableMap<in Y, X?>` used as `T3<Int, String>` is
//! `MutableMap<in String, Int?>`. Dropping the `in` reports an invariant key.

use super::common;

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

#[test]
fn a_typealias_keeps_variance_on_its_parameters() {
    let source = r#"
        import kotlin.reflect.typeOf

        typealias T1 = String
        typealias T2<X> = List<X>
        typealias T3<X, Y> = MutableMap<in Y, X?>
        typealias OutList<X> = List<out X>

        fun box(): String {
            val plain = typeOf<T1>().toString()
            if (plain != "kotlin.String") return plain
            val list = typeOf<T2<Any>>().toString()
            if (list != "kotlin.collections.List<kotlin.Any>") return list
            val projected = typeOf<T3<Int, T1>>().toString()
            if (projected != "kotlin.collections.MutableMap<in kotlin.String, kotlin.Int?>") {
                return projected
            }
            val covariant = typeOf<OutList<String>>().toString()
            if (covariant != "kotlin.collections.List<out kotlin.String>") return covariant
            return "OK"
        }
    "#;
    let reflect = reflect_jar();
    let reference =
        common::kotlinc_box_result_with_classpath(source, std::slice::from_ref(&reflect));
    assert_eq!(
        common::Fixture::new().with_reflect().run_box(source),
        reference
    );
    assert_eq!(reference, "OK");
}

#[test]
fn an_opposite_typealias_projection_is_rejected() {
    // The conflict is reported on the application that composed it. A nullable use keeps `?`
    // inside the type quotes; a nested alias does not add the outer use's `?`.
    let source = r#"
        class Box<T>
        typealias OutAlias<T> = Box<out T>
        typealias InAlias<T> = Box<in T>
        typealias Wrap<T> = Box<T>
        val a: OutAlias<in String>? = null
        val b: InAlias<out String>? = null
        val x: Wrap<OutAlias<in String>>? = null
    "#;
    common::assert_messages_match_kotlinc(source);
}

#[test]
fn a_same_direction_typealias_projection_is_kept() {
    let source = r#"
        class Box<T>
        typealias OutAlias<T> = Box<out T>
        typealias InAlias<T> = Box<in T>
        fun takeOut(x: OutAlias<out String>) = x
        fun takeIn(x: InAlias<in String>) = x
        fun takeStar(x: OutAlias<*>) = x
        fun takeInv(x: OutAlias<String>) = x
    "#;
    common::assert_accepted_like_kotlinc(source);
}
