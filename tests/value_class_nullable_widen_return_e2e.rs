//! A NON-null `@JvmInline value class` value returned where the SAME value class's NULLABLE form is
//! declared (`fun resolve(di): AppId? = when (di) { … di.applicationId … byDep(…) … }`, every branch a
//! non-null `AppId`) must compile. A non-null value class is represented UNBOXED (its underlying), the
//! nullable form BOXED (the wrapper), so the checker rejected the widening ("return type mismatch:
//! expected 'AppId', actual 'AppId'"). The checker now accepts it in a RETURN position, and the
//! value-classes emit pass boxes each unboxed tail (a value-class field read, a member/local call
//! returning the underlying) into the wrapper — leaving `null` and already-boxed tails alone. Works for a
//! classpath value class (it is in the erasure map, so its `box-impl` is emitted). Round-tripped on the JVM.
use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class AppId(val value: String = \"def\")\n\
    sealed interface DI {\n\
        data class External(val applicationId: AppId) : DI\n\
        data class Prov(val deployable: String) : DI\n\
        data class Managed(val applicationId: AppId) : DI\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn non_null_value_class_widens_to_nullable_return() {
    // Every `when` branch is a non-null `AppId` (a field read, or a member call returning the unboxed
    // underlying), flowing into the declared `AppId?` return — each must be boxed to the wrapper.
    const MAIN: &str = "import lib.AppId\nimport lib.DI\n\
        class R {\n\
            fun resolve(di: DI): AppId? = when (di) {\n\
                is DI.External -> di.applicationId\n\
                is DI.Prov -> byDep(di.deployable)\n\
                is DI.Managed -> di.applicationId\n\
            }\n\
            fun byDep(d: String): AppId = AppId(\"app-$d\")\n\
        }\n\
        fun box(): String {\n\
            val r = R()\n\
            val a = r.resolve(DI.External(AppId(\"ext\")))?.value ?: \"null\"\n\
            val b = r.resolve(DI.Prov(\"dep\"))?.value ?: \"null\"\n\
            return if (a == \"ext\" && b == \"app-dep\") \"OK\" else \"FAIL:$a|$b\"\n\
        }\n";
    assert_eq!(
        run("vcwiden", MAIN)
            .expect("non-null value class widening to a nullable return compiles + runs"),
        "OK"
    );
}

#[test]
fn nullable_value_class_return_with_null_branch() {
    // A genuine `null` branch must pass through as `null`, while the non-null branches box.
    const MAIN: &str = "import lib.AppId\n\
        class R2 {\n\
            fun pick(b: Int): AppId? = when (b) {\n\
                0 -> AppId(\"zero\")\n\
                1 -> mk()\n\
                else -> null\n\
            }\n\
            fun mk(): AppId = AppId(\"made\")\n\
        }\n\
        fun box(): String {\n\
            val r = R2()\n\
            val s = \"${r.pick(0)?.value ?: \"N\"}|${r.pick(1)?.value ?: \"N\"}|${r.pick(2)?.value ?: \"N\"}\"\n\
            return if (s == \"zero|made|N\") \"OK\" else \"FAIL:$s\"\n\
        }\n";
    assert_eq!(
        run("vcwiden_null", MAIN)
            .expect("nullable value-class return with a null branch compiles + runs"),
        "OK"
    );
}

#[test]
fn value_class_widen_from_guard_clause_return() {
    // A NON-TAIL `return` (a guard clause) of a non-null value class in a nullable-VC-return function must
    // also be boxed — the return-boxing walks every `return`, not only the tail. Previously a guard-clause
    // return left the value unboxed (VerifyError / ClassCastException).
    const MAIN: &str = "import lib.AppId\n\
        class R {\n\
            fun g(s: String): AppId = AppId(s)\n\
            fun f(neg: Boolean): AppId? {\n\
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
        run("vcwiden_guard", MAIN)
            .expect("value-class widening from a guard-clause return compiles + runs"),
        "OK"
    );
}
