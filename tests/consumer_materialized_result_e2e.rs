//! A generic call's erased reference result is narrowed where its consumer needs the narrower type,
//! as kotlinc materializes it at the consumer's type: an `Any?` parameter, an `Any` local, or a
//! structural or identity comparison takes the erased `Object` as it is, and a `CharSequence` local
//! casts to `CharSequence`, not to the substituted `String`. Provider (Java) results follow the same
//! rule as source declarations.
use super::common::{self, compare_with_kotlinc_plugin};
use std::path::PathBuf;

/// A generic Java class compiled by javac, so its `get` is a provider declaration.
fn holder_classes() -> PathBuf {
    let source = "package fixtures;\n\
                  public class Holder<T> {\n\
                  \x20   private final T value;\n\
                  \x20   public Holder(T value) { this.value = value; }\n\
                  \x20   public T get() { return value; }\n\
                  }\n";
    common::javac_compile(&[("Holder.java".into(), source.into())], &[])
        .expect("javac is provisioned")
        .0
}

const SOURCE: &str = "import fixtures.Holder\n\
    class Box<T>(val v: T) { fun get(): T = v }\n\
    class Named(val n: Int)\n\
    var sink: Any? = \"none\"\n\
    fun take(a: Any?) { sink = a }\n\
    fun argument(b: Box<String>) = take(b.get())\n\
    fun local(b: Box<String>): Any { val x: Any = b.get(); return x }\n\
    fun equality(b: Box<String>) = \"a\" == b.get()\n\
    fun identity(b: Box<String>, o: Any) = o === b.get()\n\
    fun widened(b: Box<String>): CharSequence { val c: CharSequence = b.get(); return c }\n\
    fun member(b: Box<Named>) = b.get().n\n\
    fun javaArgument(r: Holder<String>) = take(r.get())\n\
    fun javaEquality(r: Holder<String>) = r.get() == \"a\"\n";

#[test]
fn an_erased_result_is_cast_only_to_its_consumers_type() {
    let built = compare_with_kotlinc_plugin(
        "ConsumerMaterializedResult",
        SOURCE,
        "ConsumerMaterializedResultKt",
        &[common::stdlib_jar(), holder_classes()],
        "25",
        &[],
    )
    .expect("the reference kotlinc is provisioned");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "ConsumerMaterializedResultKt differs from kotlinc\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
        built.reference,
        built.krusty
    );
}

#[test]
fn an_erased_result_reaches_its_consumer_intact() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val b = Box(\"a\")\n\
         \x20   argument(b)\n\
         \x20   if (sink !== b.v) return \"argument\"\n\
         \x20   if (local(b) !== b.v) return \"local\"\n\
         \x20   if (!equality(b) || equality(Box(\"b\"))) return \"equality\"\n\
         \x20   if (!identity(b, b.v)) return \"identity\"\n\
         \x20   if (widened(b) !== b.v) return \"widened\"\n\
         \x20   if (member(Box(Named(7))) != 7) return \"member\"\n\
         \x20   val r = Holder(\"a\")\n\
         \x20   javaArgument(r)\n\
         \x20   if (sink !== r.get()) return \"javaArgument\"\n\
         \x20   if (!javaEquality(r)) return \"javaEquality\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let actual = common::compile_and_run_box(
        &src,
        "consumer_materialized_result",
        &[common::stdlib_jar(), holder_classes()],
        None,
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
