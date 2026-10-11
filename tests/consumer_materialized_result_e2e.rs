//! A generic call's erased reference result is narrowed where its consumer needs the narrower type,
//! as kotlinc materializes it at the consumer's type: an `Any?` parameter, an `Any` local, or a
//! structural or identity comparison takes the erased `Object` as it is, and a `CharSequence` local
//! casts to `CharSequence`, not to the substituted `String`. Provider (Java) results follow the same
//! rule as source declarations, and so does a function value's `invoke`.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};
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
    fun javaEquality(r: Holder<String>) = r.get() == \"a\"\n\
    fun invoked(g: () -> String) = take(g())\n\
    fun invokedEquality(g: () -> String) = \"a\" == g()\n\
    fun invokedLocal(g: () -> String): CharSequence { val c: CharSequence = g(); return c }\n";

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

/// A later branchy operand leaves the operands before it on the stack, as kotlinc does. The erased
/// result stays `Object` there and is narrowed only for a consumer that needs `String`, so each
/// method's casts match.
const BRANCHY: &str = "class Box<T>(val v: T) { fun get(): T = v }\n\
    class Pairing(val a: Any?, val b: String)\n\
    var sink: Any? = \"none\"\n\
    fun take(a: Any?, b: String) { sink = a }\n\
    fun needs(a: String, b: String) { sink = a }\n\
    fun branchyArgument(b: Box<String>, flag: Boolean) = take(b.get(), if (flag) \"a\" else \"b\")\n\
    fun branchyNarrowed(b: Box<String>, flag: Boolean) = needs(b.get(), if (flag) \"a\" else \"b\")\n\
    fun branchyEquality(b: Box<String>, flag: Boolean) = b.get() == (if (flag) \"a\" else \"b\")\n\
    fun branchyConstructor(b: Box<String>, flag: Boolean) = Pairing(b.get(), if (flag) \"a\" else \"b\")\n";

#[test]
fn a_spilled_erased_result_is_cast_only_for_its_consumer() {
    let built = compare_with_kotlinc_plugin(
        "BranchyConsumer",
        BRANCHY,
        "BranchyConsumerKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("the reference kotlinc is provisioned");
    let casts = |disassembly: &str, method: &str| {
        method_instructions(disassembly, method)
            .into_iter()
            .filter_map(|row| {
                row.split_once(": ")
                    .map(|(_, code)| code.to_string())
                    .filter(|code| code.starts_with("checkcast"))
            })
            .collect::<Vec<_>>()
    };
    for (method, expected) in [
        ("branchyArgument(", 0),
        ("branchyNarrowed(", 1),
        ("branchyEquality(", 0),
        ("branchyConstructor(", 0),
    ] {
        let reference = casts(&built.reference, method);
        assert_eq!(reference.len(), expected, "kotlinc {method}: {reference:?}");
        assert_eq!(casts(&built.krusty, method), reference, "{method}");
    }
}

#[test]
fn a_spilled_erased_result_reaches_its_consumer_intact() {
    let src = format!(
        "{BRANCHY}\
         fun box(): String {{\n\
         \x20   val b = Box(\"x\")\n\
         \x20   branchyArgument(b, true)\n\
         \x20   if (sink !== b.v) return \"argument\"\n\
         \x20   branchyNarrowed(b, false)\n\
         \x20   if (sink !== b.v) return \"narrowed\"\n\
         \x20   if (!branchyEquality(Box(\"a\"), true) || branchyEquality(b, false)) return \"equality\"\n\
         \x20   if (branchyConstructor(b, true).a !== b.v) return \"constructor\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let actual = common::compile_and_run_box(
        &src,
        "branchy_consumer",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
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
         \x20   invoked {{ \"i\" }}\n\
         \x20   if (sink != \"i\") return \"invoked\"\n\
         \x20   if (!invokedEquality {{ \"a\" }}) return \"invokedEquality\"\n\
         \x20   if (invokedLocal {{ \"c\" }} != \"c\") return \"invokedLocal\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let actual = common::compile_and_run_box(
        &src,
        "consumer_materialized_result",
        &[common::stdlib_jar(), holder_classes()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
