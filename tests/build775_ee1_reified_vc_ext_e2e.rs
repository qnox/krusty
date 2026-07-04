//! build.775 ee1: a `reified` extension overloading a member whose 1st param is a value class. The
//! library declares `fun <T:Any> getFor(id: Aid, t: KClass<T>): T` on interface `Core` (aliased
//! `Reg`), plus an `inline fun <reified T:Any> Reg.getFor(id: Aid): T = getFor(id, T::class)`. Calling
//! `r.getFor<Prov>(id).go()` typed the result `Any` → "unresolved method 'go' on kotlin/Any". The
//! reified-ext return must bind to the reified `T` so the following member resolves.
use super::common;

const LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    @JvmInline value class Aid(val v: String)\n\
    interface Prov { fun go(): String }\n\
    object ProvImpl : Prov { override fun go(): String = \"OK\" }\n\
    interface Core { fun <T : Any> getFor(id: Aid, t: KClass<T>): T }\n\
    typealias Reg = Core\n\
    inline fun <reified T : Any> Reg.getFor(id: Aid): T = getFor(id, T::class)\n\
    object R : Core {\n\
        @Suppress(\"UNCHECKED_CAST\")\n\
        override fun <T : Any> getFor(id: Aid, t: KClass<T>): T = ProvImpl as T\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn reified_vc_param_ext_overload_return_member() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String { val r: Reg = R; val id = Aid(\"x\"); return r.getFor<Prov>(id).go() }\n";
    assert_eq!(
        run("ee1_775", MAIN).expect("reified vc-param ext overload return member"),
        "OK"
    );
}
