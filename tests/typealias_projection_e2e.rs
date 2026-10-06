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
fn one_alias_parameter_can_fill_several_target_arguments() {
    const SOURCE: &str = r#"
        interface ValueShape<V>
        interface SelectedShape<V> : ValueShape<V>
        open class Cell<V, S : ValueShape<V>>
        typealias SelectedCell<V> = Cell<V, out SelectedShape<V>>

        data class Holder<P>(val value: SelectedCell<P>)

        fun box(): String = "OK"
    "#;

    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "RepeatedAliasArgument"),
        "OK"
    );
}

#[test]
fn an_opposite_typealias_projection_is_rejected() {
    // Each opposite use-site keyword is its own diagnostic, in source order, quoting the
    // expansion of that application (a nullable use keeps `?` inside the type quotes).
    let source = r#"
        class Box<T>
        typealias OutAlias<T> = Box<out T>
        typealias InAlias<T> = Box<in T>
        typealias Wrap<T> = Box<T>
        val a: OutAlias<in String>? = null
        val b: InAlias<out String>? = null
        val x: Wrap<OutAlias<in String>>? = null
    "#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn a_nearer_classifier_shadows_a_typealias() {
    let source = r#"
        class Target<T>
        typealias Pick<T> = Target<out T>

        fun use() {
            class Pick<T>
            val x: Pick<String>? = null
        }
    "#;
    common::assert_accepted_like_kotlinc(source);
}

#[test]
fn a_same_package_classifier_does_not_inherit_a_same_target_star_alias() {
    const LIBRARY: &[(&str, &str)] = &[
        ("Pick.kt", "package app\nclass Pick<T>\n"),
        (
            "Alias.kt",
            "package imported\ntypealias Pick<T> = app.Pick<out T>\n",
        ),
    ];
    const SOURCE: &str = "package app\n\
        import imported.*\n\
        val value: Pick<String>? = null\n";
    let result = common::metadata_diff_against_kotlinc_lib(
        "SameTargetShadow",
        LIBRARY,
        SOURCE,
        "app/SameTargetShadowKt",
    )
    .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
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
