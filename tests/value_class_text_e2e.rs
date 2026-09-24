//! A non-null value class rendered as text or hashed goes through its static `-impl`.
//!
//! kotlinc renders a value class in a string template — including a data class's `toString` over a
//! value-class property — as `X.toString-impl(carrier)` appended at the value class's own type
//! (`append(Object)`), and calls `x.toString()`/`x.hashCode()` as the static `-impl` too. krusty
//! boxed the carrier with `box-impl` first and dispatched on the box.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "@JvmInline value class N(val n: Int)\n\
    @JvmInline value class S(val s: String)\n\
    data class Holder(val n: N, val s: S)\n\
    fun template(n: N) = \"x=$n\"\n\
    fun text(s: S) = s.toString()\n\
    fun hash(n: N) = n.hashCode()\n";

#[test]
fn value_class_text_matches_kotlinc() {
    for (class, members) in [
        (
            "ValueClassTextKt",
            &["String template-", "String text-", "int hash-"][..],
        ),
        ("Holder", &["java.lang.String toString()"][..]),
    ] {
        let Some(built) = compare_with_kotlinc_plugin(
            "ValueClassText",
            SOURCE,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        for member in members {
            let reference = method_instructions(&built.reference, member);
            assert!(!reference.is_empty(), "{class}: {member} not found");
            assert_eq!(
                method_instructions(&built.krusty, member),
                reference,
                "{class}: {member}"
            );
        }
    }
}

#[test]
fn value_class_text_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (template(N(3)) != \"x=N(n=3)\") return \"FAIL template \" + template(N(3))\n\
             \x20   if (text(S(\"a\")) != \"S(s=a)\") return \"FAIL text\"\n\
             \x20   if (hash(N(3)) != 3.hashCode()) return \"FAIL hash\"\n\
             \x20   val holder = Holder(N(1), S(\"b\"))\n\
             \x20   return if (holder.toString() == \"Holder(n=N(n=1), s=S(s=b))\") \"OK\" else \"FAIL \" + holder\n\
             }}\n"
        ),
        "value-class text",
    );
}
