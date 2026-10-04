//! kotlinc renames a function whose signature mentions a value class (`lamP-txdesME`, a value-class
//! member's `m-impl`, its `init` block's `constructor-impl`) before it lifts the lambdas and local
//! functions declared in it. Each lifted method is therefore named after that JVM name, with `-`
//! spelled `_`: `lamP_txdesME$lambda$0`, `local_txdesME$inner`, `m_impl$lambda$0`. The renamed
//! function also numbers its lambdas on its own: `o(Tag)` does not share the counter of `o(Int)`.
//! Classes declared in such a function (an anonymous object, a local class) keep the source name.
//!
//! krusty named every lifted method after the source name (`lamP$lambda$0`).
use super::common;

const SOURCE: &str = "@JvmInline value class Tag(val s: String)\n\
    fun lamP(t: Tag): () -> Any = { t }\n\
    fun nested(t: Tag): () -> () -> Any = { { t } }\n\
    fun local(t: Tag): Any {\n\
    \x20   fun inner(): Any = t\n\
    \x20   return inner()\n\
    }\n\
    fun localLambda(t: Tag): Any {\n\
    \x20   fun inner(): () -> Any = { t }\n\
    \x20   return inner()()\n\
    }\n\
    fun o(i: Int): () -> Any = { i }\n\
    fun o(t: Tag): () -> Any = { t }\n\
    fun o(s: String): () -> Any = { s }\n\
    fun anon(t: Tag): Any = object { override fun toString(): String = t.s }\n\
    fun localClass(t: Tag): Any {\n\
    \x20   class L { fun g(): Any = t }\n\
    \x20   return L().g()\n\
    }\n\
    fun plain(): Tag {\n\
    \x20   val f = { 1 }\n\
    \x20   f()\n\
    \x20   return Tag(\"\")\n\
    }\n\
    suspend fun sus(t: Tag): () -> Any = { t }\n\
    fun Tag.ext(): () -> Any = { this }\n\
    internal fun intl(t: Tag): () -> Any = { t }\n\
    val Tag.prop: () -> Any get() = { this }\n\
    class Holder { fun m(t: Tag): () -> Any = { t } }\n\
    @JvmInline value class V(val s: String) {\n\
    \x20   init {\n\
    \x20       val f = { s.length }\n\
    \x20       f()\n\
    \x20   }\n\
    \x20   fun m(): () -> Any = { s }\n\
    \x20   fun n(t: Tag): () -> Any = { t }\n\
    \x20   val p: () -> Any get() = { s }\n\
    }\n";

/// The lifted methods kotlinc writes, by class, as `javap -p` declares them, in class-file order.
const LIFTED: &[(&str, &str)] = &[
    (
        "MangledKt",
        "private static final java.lang.Object local_txdesME$inner(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final kotlin.jvm.functions.Function0<java.lang.Object> localLambda_txdesME$inner(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object lamP_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object nested_txdesME$lambda$0$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final kotlin.jvm.functions.Function0 nested_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object o$lambda$0(int);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object o_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object o$lambda$1(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final int plain$lambda$0();",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object sus_47cE_Rs$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object ext_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object intl_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object getProp_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "MangledKt",
        "private static final java.lang.Object localLambda_txdesME$inner$lambda$0(java.lang.String);",
    ),
    (
        "Holder",
        "private static final java.lang.Object m_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object m_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object n_txdesME$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object getP_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final int constructor_impl$lambda$0(java.lang.String);",
    ),
];

#[test]
fn a_lambda_in_a_value_class_mangled_function_is_named_after_its_jvm_name_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Mangled.kt", SOURCE)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>(),
        "kotlinc's class set"
    );
    for class in ["MangledKt", "Holder", "V"] {
        let (reference, krusty) = classes
            .method_declarations(class)
            .unwrap_or_else(|| panic!("both compilers write {class}"));
        let lifted = |declarations: &[String]| {
            declarations
                .iter()
                .filter(|declaration| declaration.starts_with("private static final"))
                .filter(|declaration| declaration.contains('$'))
                .cloned()
                .collect::<Vec<_>>()
        };
        let expected = LIFTED
            .iter()
            .filter(|(owner, _)| *owner == class)
            .map(|(_, declaration)| declaration.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            lifted(&reference),
            expected,
            "kotlinc's lifted methods of {class}"
        );
        if class == "V" {
            // krusty places a value class's lifted methods ahead of its synthesized members, and
            // the `init` block's first; kotlinc writes them last. That order is apart from naming.
            let mut krusty = lifted(&krusty);
            let mut expected = expected;
            krusty.sort();
            expected.sort();
            assert_eq!(krusty, expected, "krusty's lifted methods of {class}");
        } else {
            assert_eq!(krusty, reference, "kotlinc's methods of {class}");
        }
    }
    // Every lifted method matches whole, its local-variable table included: a value-class lambda
    // that captures its receiver names it `$arg0` in a member or accessor and `$tmp0` in the `init`
    // block, after the value kotlinc's value-class statics realize that receiver as.
    for (class, declaration) in LIFTED {
        let (reference, krusty) = classes.method_listing(class, declaration);
        assert_eq!(krusty, reference, "{class}: {declaration}");
    }
}
