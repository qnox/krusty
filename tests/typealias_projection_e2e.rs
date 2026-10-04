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
