//! A NON-null `@JvmInline value class` value returned where the SAME value class's NULLABLE form is
//! declared (`fun resolve(input): SampleId? = when (input) { … input.id … fromSeed(…) … }`, every
//! branch a non-null `SampleId`) must compile. A non-null value class is represented UNBOXED (its
//! underlying), the
//! nullable form BOXED (the wrapper), so the checker rejected the widening ("return type mismatch:
//! expected 'SampleId', actual 'SampleId'"). The checker now accepts it in a RETURN position, and the
//! value-classes emit pass boxes each unboxed tail (a value-class field read, a member/local call
//! returning the underlying) into the wrapper — leaving `null` and already-boxed tails alone. Works for a
//! classpath value class (it is in the erasure map, so its `box-impl` is emitted). Round-tripped on the JVM.
use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class SampleId(val value: String = \"default\")\n\
    sealed interface Input {\n\
        data class Stored(val id: SampleId) : Input\n\
        data class Derived(val seed: String) : Input\n\
        data class Cached(val id: SampleId) : Input\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(jdk.as_path()))
}

/// Reference-compiled dependency variant: these cases consume kotlinc-emitted metadata
/// shapes krusty does not produce yet (see `common::compile_lib_ref`).
fn run_ref(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let lo = common::compile_lib_ref(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(jdk.as_path()))
}

#[test]
fn non_null_value_class_widens_to_nullable_return() {
    // Every `when` branch is a non-null `SampleId` (a field read, or a member call returning the
    // unboxed underlying), flowing into the declared `SampleId?` return — each must be boxed.
    const MAIN: &str = "import lib.SampleId\nimport lib.Input\n\
        class R {\n\
            fun resolve(input: Input): SampleId? = when (input) {\n\
                is Input.Stored -> input.id\n\
                is Input.Derived -> fromSeed(input.seed)\n\
                is Input.Cached -> input.id\n\
            }\n\
            fun fromSeed(seed: String): SampleId = SampleId(\"sample-$seed\")\n\
        }\n\
        fun box(): String {\n\
            val r = R()\n\
            val a = r.resolve(Input.Stored(SampleId(\"stored\")))?.value ?: \"null\"\n\
            val b = r.resolve(Input.Derived(\"seed\"))?.value ?: \"null\"\n\
            return if (a == \"stored\" && b == \"sample-seed\") \"OK\" else \"FAIL:$a|$b\"\n\
        }\n";
    assert_eq!(
        run_ref("vcwiden_anonymized", MAIN)
            .expect("non-null value class widening to a nullable return compiles + runs"),
        "OK"
    );
}

#[test]
fn nullable_value_class_return_with_null_branch() {
    // A genuine `null` branch must pass through as `null`, while the non-null branches box.
    const MAIN: &str = "import lib.SampleId\n\
        class R2 {\n\
            fun pick(b: Int): SampleId? = when (b) {\n\
                0 -> SampleId(\"zero\")\n\
                1 -> mk()\n\
                else -> null\n\
            }\n\
            fun mk(): SampleId = SampleId(\"made\")\n\
        }\n\
        fun box(): String {\n\
            val r = R2()\n\
            val s = \"${r.pick(0)?.value ?: \"N\"}|${r.pick(1)?.value ?: \"N\"}|${r.pick(2)?.value ?: \"N\"}\"\n\
            return if (s == \"zero|made|N\") \"OK\" else \"FAIL:$s\"\n\
        }\n";
    assert_eq!(
        run("vcwiden_null_anonymized", MAIN)
            .expect("nullable value-class return with a null branch compiles + runs"),
        "OK"
    );
}

#[test]
fn value_class_widen_from_guard_clause_return() {
    // A NON-TAIL `return` (a guard clause) of a non-null value class in a nullable-VC-return function must
    // also be boxed — the return-boxing walks every `return`, not only the tail. Previously a guard-clause
    // return left the value unboxed (VerifyError / ClassCastException).
    const MAIN: &str = "import lib.SampleId\n\
        class R {\n\
            fun g(s: String): SampleId = SampleId(s)\n\
            fun f(neg: Boolean): SampleId? {\n\
                if (neg) return g(\"neg\")\n\
                return null\n\
            }\n\
        }\n\
        fun box(): String {\n\
            val r = R()\n\
            val s = \"${r.f(true)?.value ?: \"N\"}|${r.f(false)?.value ?: \"N\"}\"\n\
            return if (s == \"neg|N\") \"OK\" else \"FAIL:$s\"\n\
        }\n";
    assert_eq!(
        run("vcwiden_guard_anonymized", MAIN)
            .expect("value-class widening from a guard-clause return compiles + runs"),
        "OK"
    );
}
