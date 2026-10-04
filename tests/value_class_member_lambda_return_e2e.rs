//! A lambda, local function, or callable-reference adapter written inside a value-class member is
//! lifted to a private static function of the value class. It is not a member that runs on the
//! box: a captured value (the value-class receiver `this` among them) reaches it as an ordinary
//! carrier parameter, so each reference boundary in its body boxes the carrier exactly as it does
//! in a lambda lifted into an ordinary class. A return through `Any`, `Any?`, or an interface the
//! value class implements, an `Any` argument, and an `Any` local all hand over the box.
//!
//! krusty treated the lifted function as a member of the value class whose body runs on the box,
//! skipped every boundary in it, and returned the raw `String` carrier where kotlinc calls
//! `Tag.box-impl`.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "interface Marker\n\
    @JvmInline value class Tag(val s: String) : Marker\n\
    fun take(a: Any): Boolean = a is Tag\n\
    fun takeTag(a: Tag?): Boolean = a != null\n\
    @JvmInline value class V(val s: String) {\n\
    \x20   fun n(t: Tag): () -> Any = { t }\n\
    \x20   fun nullable(t: Tag): () -> Any? = { t }\n\
    \x20   fun marker(t: Tag): () -> Marker = { t }\n\
    \x20   fun self(): () -> Any = { this }\n\
    \x20   fun argument(t: Tag): () -> Boolean = { take(t) }\n\
    \x20   fun nullableArgument(t: Tag): () -> Boolean = { takeTag(t) }\n\
    \x20   fun stored(t: Tag): () -> Any {\n\
    \x20       return {\n\
    \x20           val a: Any = t\n\
    \x20           a\n\
    \x20       }\n\
    \x20   }\n\
    \x20   fun local(t: Tag): Any? {\n\
    \x20       fun g(): Any? = t\n\
    \x20       return g()\n\
    \x20   }\n\
    }\n";

/// The lifted methods of `V`, by a marker unique to each declaration.
const METHODS: &[&str] = &[
    " n_txdesME$lambda$0(",
    " nullable_txdesME$lambda$0(",
    " marker_txdesME$lambda$0(",
    " self_impl$lambda$0(",
    " argument_txdesME$lambda$0(",
    " nullableArgument_txdesME$lambda$0(",
    " stored_txdesME$lambda$0(",
    " local_txdesME$g(",
];

#[test]
fn a_lambda_lifted_into_a_value_class_boxes_its_reference_boundaries_like_kotlinc() {
    let built = compare_with_kotlinc_plugin(
        "MemberLambda",
        SOURCE,
        "V",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in METHODS {
        let reference = method_instructions(&built.reference, method);
        assert!(!reference.is_empty(), "kotlinc has no {method}");
        assert_eq!(
            method_instructions(&built.krusty, method),
            reference,
            "{method}\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
            built.reference,
            built.krusty
        );
    }
}

#[test]
fn a_lambda_lifted_into_a_value_class_returns_the_box() {
    let source = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val v = V(\"v\")\n\
         \x20   if (v.n(Tag(\"a\"))() !is Tag) return \"n\"\n\
         \x20   if (v.nullable(Tag(\"a\"))() !is Tag) return \"nullable\"\n\
         \x20   if (v.marker(Tag(\"a\"))() !is Tag) return \"marker\"\n\
         \x20   if (v.self()() !is V) return \"self\"\n\
         \x20   if (!v.argument(Tag(\"a\"))()) return \"argument\"\n\
         \x20   if (!v.nullableArgument(Tag(\"a\"))()) return \"nullableArgument\"\n\
         \x20   if (v.stored(Tag(\"a\"))() !is Tag) return \"stored\"\n\
         \x20   if (v.local(Tag(\"a\")) !is Tag) return \"local\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(&source, "member_lambda_return"),
        "OK"
    );
}
