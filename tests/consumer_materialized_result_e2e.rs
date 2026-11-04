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

// PLATFORM: JVM result-slot casts and complete classfiles; ordinary Kotlin runtime is also
// checked against kotlinc through the shared runner.
const RETURN_CONSUMERS: &str = r#"
var calls = 0
fun <T> supplied(value: T): T { calls++; return value }
fun widenedReturn(): CharSequence = supplied<String>("OK")
fun erasedReturn(): Any = supplied<String>("OK")
fun storedReturn(): CharSequence {
    val value = supplied<String>("OK")
    return value
}
fun explicitReturn(): CharSequence = supplied<Any>("OK") as String
fun assertedReturn(): CharSequence = supplied<String?>("OK")!!
fun box(): String {
    if (widenedReturn() != "OK") return "widened"
    if (erasedReturn() != "OK") return "erased"
    if (storedReturn() != "OK") return "stored"
    if (explicitReturn() != "OK") return "explicit"
    if (assertedReturn() != "OK") return "asserted"
    return if (calls == 5) "OK" else "evaluation"
}
"#;

#[test]
fn erased_generic_return_consumers_match_complete_kotlinc_classfiles() {
    let classes =
        common::classes_against_kotlinc_module(&[("GenericReturnConsumers.kt", RETURN_CONSUMERS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn erased_generic_returns_preserve_materialization_and_single_evaluation() {
    assert_eq!(common::kotlinc_box_result(RETURN_CONSUMERS), "OK");
    assert_eq!(
        common::compile_and_run_box(
            RETURN_CONSUMERS,
            "GenericReturnConsumers",
            &[common::stdlib_jar()],
            Some(&common::jdk_modules()),
        )
        .expect("generic return consumers compile and run"),
        "OK"
    );
}

/// A result erased to a declared bound (`<T : Base>` returns `Base`) follows the same rule as an
/// erased `Object`: kotlinc narrows it only for a consumer that needs the narrower type. An identity
/// or null comparison, `areEqual`, an `Any` or `Base` consumer take the slot unchanged; a `Marker`
/// consumer casts once to `Marker`, never to the substituted `Both` first. A dispatch receiver
/// narrows to the class the call names (`Both.base()`), even when the member is declared on `Base`.
mod bound_results {
    use super::common;

    const DECLARATIONS: &str = "package bounds\n\
        \n\
        interface Marker\n\
        \n\
        open class Base { open fun base(): String = \"B\" }\n\
        \n\
        class Both : Base(), Marker { fun both(): String = \"O\" }\n\
        \n\
        class Simple<T : Base>(val value: T) { fun get(): T = value }\n\
        \n\
        class Pair<T>(val value: T) where T : Marker, T : Base {\n\
        \x20   fun get(): T = value\n\
        \x20   fun <R> echo(r: R): R where R : Marker, R : Base = r\n\
        }\n\
        \n\
        class Plain<T>(val value: T) { fun get(): T = value }\n\
        \n\
        class Maybe<T : Base>(val value: T?) { fun get(): T? = value }\n\
        \n\
        fun <T : Base> pass(value: T): T = value\n";

    const USES: &str = "package bounds\n\
        \n\
        fun same(s: Simple<Both>, b: Both): Boolean = s.get() === b\n\
        \n\
        fun notSame(s: Simple<Both>, b: Both): Boolean = s.value !== b\n\
        \n\
        fun intersection(p: Pair<Both>, b: Both): Boolean =\n\
        \x20   p.get() === b && p.value === b && p.echo(b) === b\n\
        \n\
        fun topLevel(b: Both): Boolean = pass(b) === b\n\
        \n\
        fun absent(m: Maybe<Both>): Boolean = m.get() == null\n\
        \n\
        fun present(m: Maybe<Both>): Boolean = m.value != null\n\
        \n\
        fun equal(s: Simple<Both>, b: Both): Boolean = s.get() == b\n\
        \n\
        fun asAny(s: Simple<Both>): Any = s.get()\n\
        \n\
        fun asBound(s: Simple<Both>): Base = s.value\n\
        \n\
        fun asMarker(p: Pair<Both>): Marker = p.get()\n\
        \n\
        fun declared(s: Simple<Both>): String = s.get().base() + s.value.base()\n\
        \n\
        fun erasedDeclared(p: Plain<Both>): String = p.get().base() + p.value.base()\n\
        \n\
        fun own(s: Simple<Both>): String = s.get().both() + s.value.both()\n\
        \n\
        fun box(): String {\n\
        \x20   val b = Both()\n\
        \x20   val s = Simple(b)\n\
        \x20   val p = Pair(b)\n\
        \x20   if (!same(s, b) || notSame(s, b)) return \"identity\"\n\
        \x20   if (!intersection(p, b) || !topLevel(b)) return \"intersection\"\n\
        \x20   if (absent(Maybe(b)) || !absent(Maybe<Both>(null))) return \"absent\"\n\
        \x20   if (!present(Maybe(b)) || present(Maybe<Both>(null))) return \"present\"\n\
        \x20   if (!equal(s, b) || asAny(s) !== b || asBound(s) !== b) return \"consumers\"\n\
        \x20   if (asMarker(p) !== b) return \"marker\"\n\
        \x20   if (declared(s) != \"BB\" || erasedDeclared(Plain(b)) != \"BB\") return \"declared\"\n\
        \x20   return if (own(s) == \"OO\") \"OK\" else \"own\"\n\
        }\n";

    /// The declarations and their uses as one default-package file.
    fn one_file() -> String {
        format!(
            "{}\n{}",
            DECLARATIONS.replacen("package bounds\n", "", 1),
            USES.replacen("package bounds\n", "", 1)
        )
    }

    const CONSUMERS: [&str; 14] = [
        "same",
        "notSame",
        "intersection",
        "topLevel",
        "absent",
        "present",
        "equal",
        "asAny",
        "asBound",
        "asMarker",
        "declared",
        "erasedDeclared",
        "own",
        "box",
    ];

    #[test]
    fn bound_results_in_one_file_match_kotlinc_classfiles() {
        let source = one_file();
        let classes = common::classes_against_kotlinc_module(&[("BoundResults.kt", &source)]);
        assert_eq!(classes.differences(), Vec::<String>::new());
    }

    #[test]
    fn bound_results_from_another_file_match_kotlinc_classfiles() {
        let classes = common::classes_against_kotlinc_module(&[
            ("Declarations.kt", DECLARATIONS),
            ("Uses.kt", USES),
        ]);
        assert_eq!(classes.differences(), Vec::<String>::new());
    }

    #[test]
    fn bound_results_of_a_kotlinc_dependency_match_kotlinc() {
        let consumer = USES.replacen("package bounds\n", "import bounds.*\n", 1);
        let results = common::class_bytes_diffs_against_kotlinc(
            "BoundResultUses",
            &[("Declarations.kt", DECLARATIONS)],
            &consumer,
            "BoundResultUsesKt",
            &CONSUMERS,
        )
        .expect("reference kotlinc is provisioned");
        for result in results {
            result.unwrap_or_else(|diff| panic!("{diff}"));
        }
        assert_eq!(
            common::expect_box_run_against_kotlinc(DECLARATIONS, &consumer)
                .expect("reference kotlinc is provisioned"),
            "OK"
        );
    }

    #[test]
    fn bound_results_run() {
        let source = one_file();
        assert_eq!(common::kotlinc_box_result(&source), "OK");
        assert_eq!(
            common::compile_and_run_box(
                &source,
                "BoundResults",
                &[common::stdlib_jar()],
                Some(&common::jdk_modules()),
            )
            .expect("bound results compile and run"),
            "OK"
        );
    }
}

/// A result erased to its declaration's slot keeps that slot through `!!` and string templates, as
/// kotlinc's consumers do. `!!` checks the erased value and only then narrows it for a consumer
/// that needs the narrower type, for a property read as for a call. A template part reaches
/// `String.valueOf(Object)`, `StringBuilder.append(Object)`, and an `invokedynamic` argument in
/// its erased slot; only `append(String)` narrows it, to `String`.
mod bound_result_order {
    use super::common;

    const DECLARATIONS: &str = "package order\n\
        \n\
        open class Base\n\
        \n\
        class Both : Base() { override fun toString(): String = \"both\" }\n\
        \n\
        class Simple<T : Base>(val value: T, val nul: T?) {\n\
        \x20   fun get(): T = value\n\
        \x20   fun getN(): T? = nul\n\
        }\n\
        \n\
        class Plain<T>(val value: T, val nul: T?) {\n\
        \x20   fun get(): T = value\n\
        \x20   fun getN(): T? = nul\n\
        }\n";

    const USES: &str = "package order\n\
        \n\
        fun readChecked(s: Simple<Both>): Both = s.nul!!\n\
        \n\
        fun callChecked(s: Simple<Both>): Both = s.getN()!!\n\
        \n\
        fun erasedReadChecked(p: Plain<Both>): Both = p.nul!!\n\
        \n\
        fun boundChecked(s: Simple<Both>): Base = s.nul!!\n\
        \n\
        fun identityChecked(s: Simple<Both>): Boolean = s.nul!! === s.value\n\
        \n\
        fun single(s: Simple<Both>): String = \"${s.value}\"\n\
        \n\
        fun singleCall(p: Plain<Both>): String = \"${p.get()}\"\n\
        \n\
        fun singleNullable(s: Simple<Both>): String = \"${s.nul}\"\n\
        \n\
        fun parts(s: Simple<Both>): String = \"a${s.value}b${s.get()}\"\n\
        \n\
        fun erasedParts(p: Plain<Both>): String = \"a${p.value}b${p.get()}\"\n\
        \n\
        fun stringPart(p: Plain<String>): String = \"x${p.get()}\"\n\
        \n\
        fun box(): String {\n\
        \x20   val b = Both()\n\
        \x20   val s = Simple(b, b)\n\
        \x20   val p = Plain(b, b)\n\
        \x20   if (readChecked(s) !== b || callChecked(s) !== b) return \"checked\"\n\
        \x20   if (erasedReadChecked(p) !== b || boundChecked(s) !== b) return \"erased\"\n\
        \x20   if (!identityChecked(s)) return \"identity\"\n\
        \x20   try {\n\
        \x20       readChecked(Simple(b, null))\n\
        \x20       return \"no check\"\n\
        \x20   } catch (e: NullPointerException) {}\n\
        \x20   if (single(s) != \"both\" || singleCall(p) != \"both\") return \"single\"\n\
        \x20   if (singleNullable(Simple(b, null)) != \"null\") return \"nullable\"\n\
        \x20   if (parts(s) != \"abothbboth\" || erasedParts(p) != \"abothbboth\") return \"parts\"\n\
        \x20   return if (stringPart(Plain(\"y\", null)) == \"xy\") \"OK\" else \"string\"\n\
        }\n";

    /// The declarations and their uses as one default-package file.
    fn one_file() -> String {
        format!(
            "{}\n{}",
            DECLARATIONS.replacen("package order\n", "", 1),
            USES.replacen("package order\n", "", 1)
        )
    }

    #[test]
    fn checked_and_interpolated_results_in_one_file_match_kotlinc_classfiles() {
        let source = one_file();
        let classes = common::classes_against_kotlinc_module(&[("ResultOrder.kt", &source)]);
        assert_eq!(classes.differences(), Vec::<String>::new());
    }

    /// The `StringBuilder` form below JVM 9 and the `invokedynamic` form from JVM 9 on.
    #[test]
    fn checked_and_interpolated_results_of_a_kotlinc_dependency_match_kotlinc() {
        let consumer = USES.replacen("package order\n", "import order.*\n", 1);
        for target in [None, Some(11)] {
            let classes = common::classes_against_kotlinc_lib_target(
                "ResultOrderUses",
                &[("Declarations.kt", DECLARATIONS)],
                &consumer,
                target,
            )
            .expect("reference kotlinc is provisioned");
            assert_eq!(classes.differences(), Vec::<String>::new(), "{target:?}");
        }
        assert_eq!(
            common::expect_box_run_against_kotlinc(DECLARATIONS, &consumer)
                .expect("reference kotlinc is provisioned"),
            "OK"
        );
    }

    #[test]
    fn checked_and_interpolated_results_run() {
        let source = one_file();
        assert_eq!(common::kotlinc_box_result(&source), "OK");
        assert_eq!(
            common::compile_and_run_box(
                &source,
                "ResultOrder",
                &[common::stdlib_jar()],
                Some(&common::jdk_modules()),
            )
            .expect("checked and interpolated results compile and run"),
            "OK"
        );
    }
}
