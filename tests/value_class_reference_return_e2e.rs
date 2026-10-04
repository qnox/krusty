//! A function declared to return a reference supertype of a value class (`Any`, `Any?`, or an
//! interface the class implements) takes its returned value through a reference slot, the same
//! boundary as an `Any` parameter or local. A value already in its box, such as the result of a
//! generic call substituted with the value class (`shelf[0]`), is returned
//! as it is; a carrier (a value-class parameter or local) is boxed. A lambda whose result is `Any`
//! returns through its erased `invoke` the same way.
//!
//! krusty unboxed the generic result for its value-class type and returned the raw carrier, so
//! `fun f(s: Shelf<Tag>): Any = s[0]` handed back a `String` that is not a `Tag`.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "interface Parent\n\
    interface Named : Parent\n\
    @JvmInline value class Tag(val s: String) : Named\n\
    @JvmInline value class Count(val n: Int)\n\
    class Shelf<T>(val item: T) { operator fun get(i: Int): T = item }\n\
    fun take(a: Any): Any = a\n\
    fun indexed(s: Shelf<Tag>): Any = s[0]\n\
    fun returned(s: Shelf<Tag>): Any { return s[1] }\n\
    fun called(s: Shelf<Tag>): Any = s.get(0)\n\
    fun counted(s: Shelf<Count>): Any = s[0]\n\
    fun named(s: Shelf<Tag>): Named = s[0]\n\
    fun parent(s: Shelf<Tag>): Parent = s[0]\n\
    fun nullable(s: Shelf<Tag?>): Any? = s[0]\n\
    fun argument(s: Shelf<Tag>): Any = take(s[0])\n\
    fun parameter(t: Tag): Any = t\n\
    fun countParameter(c: Count): Any = c\n\
    fun local(t: Tag): Any { val x = t; return x }\n\
    val Shelf<Tag>.first: Any get() = this[0]\n\
    fun lambda(s: Shelf<Tag>): () -> Any = { s[0] }\n\
    fun captured(t: Tag): () -> Any = { t }\n";

const METHODS: &[&str] = &[
    " indexed(",
    " returned(",
    " called(",
    " counted(",
    " named(",
    " parent(",
    " nullable(",
    " argument(",
    " parameter-",
    " countParameter-",
    " local-",
    " getFirst(",
    " lambda$lambda$0(",
];

#[test]
fn a_value_class_returned_as_a_reference_supertype_is_its_box_like_kotlinc() {
    let built = compare_with_kotlinc_plugin(
        "ReferenceReturn",
        SOURCE,
        "ReferenceReturnKt",
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
fn a_value_class_returned_as_a_reference_supertype_keeps_its_identity() {
    let source = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val s = Shelf(Tag(\"a\"))\n\
         \x20   if (indexed(s) !is Tag) return \"indexed\"\n\
         \x20   if (returned(s) !is Tag) return \"returned\"\n\
         \x20   if (called(s) !is Tag) return \"called\"\n\
         \x20   if (counted(Shelf(Count(3))) != Count(3)) return \"counted\"\n\
         \x20   if (named(s) !is Tag) return \"named\"\n\
         \x20   if (parent(s) !is Tag) return \"parent\"\n\
         \x20   if (nullable(Shelf<Tag?>(Tag(\"b\"))) != Tag(\"b\")) return \"nullable\"\n\
         \x20   if (nullable(Shelf<Tag?>(null)) != null) return \"null\"\n\
         \x20   if (argument(s) !is Tag) return \"argument\"\n\
         \x20   if (parameter(Tag(\"c\")) != Tag(\"c\")) return \"parameter\"\n\
         \x20   if (countParameter(Count(4)) !is Count) return \"countParameter\"\n\
         \x20   if (local(Tag(\"d\")) !is Tag) return \"local\"\n\
         \x20   if (s.first !is Tag) return \"first\"\n\
         \x20   if (lambda(s)() !is Tag) return \"lambda\"\n\
         \x20   if (captured(Tag(\"e\"))() !is Tag) return \"captured\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(&source, "reference_return"),
        "OK"
    );
}
