//! Assertion libraries overload one static entry point by argument type: AssertJ's
//! `<T> assertThat(T)` beside `<E> assertThat(List<? extends E>)` and `assertThat(Object)`
//! beside `assertThat(Iterable<?>)`. A nullable `List<String>?` argument fits the bare `T`
//! exactly only because `T` is inferred from it, while the flexible `(Mutable)List<out E>!`
//! parameter admits it without being equal to it. kotlinc ranks the two by their declarations and
//! selects the `List` overload; krusty took the inferred exact fit as evidence and returned the
//! generic `T` overload's result type, so the chained assertion did not resolve.

use super::common;

const ASSERTS: &str = "import java.util.List;\n\
    public class Asserts {\n\
    \x20   public static String check(Object actual) { return \"object\"; }\n\
    \x20   public static <T> String generic(T actual) { return \"variable\"; }\n\
    \x20   public static <E> int generic(List<? extends E> actual) { return 2; }\n\
    \x20   public static String text(Object actual) { return \"object\"; }\n\
    \x20   public static <T extends CharSequence> int text(T actual) { return 3; }\n\
    }\n";

const USE: &str = "val notes: Map<String, List<String>> = mapOf(\"a\" to listOf(\"x\"))\n\
    fun box(): String {\n\
    \x20   val absent: List<String>? = null\n\
    \x20   val present: List<String>? = listOf(\"x\")\n\
    \x20   if (Asserts.generic(notes[\"a\"]).inc() != 3) return \"indexed\"\n\
    \x20   if (Asserts.generic(absent).inc() != 3) return \"null\"\n\
    \x20   if (Asserts.generic(present).inc() != 3) return \"nullable\"\n\
    \x20   if (Asserts.generic(mutableListOf(\"x\")).inc() != 3) return \"mutable\"\n\
    \x20   if (Asserts.text(\"s\").inc() != 4) return \"bounded\"\n\
    \x20   if (Asserts.generic(\"s\").length != 8) return \"variable\"\n\
    \x20   return if (Asserts.check(absent) == \"object\") \"OK\" else \"object\"\n\
    }\n";

#[test]
fn a_java_static_overload_is_ranked_by_its_declaration_not_an_inferred_exact_fit() {
    assert_eq!(
        common::java_interop_box(
            "JavaStaticGenericOverload",
            &[("Asserts.java", ASSERTS)],
            USE,
        ),
        "OK"
    );
}
