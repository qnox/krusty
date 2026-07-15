//! A class with a `suspend` member that overrides a NON-GENERIC supertype method (an interface impl /
//! suspend decorator). No generic erasure is involved, so the CPS override directly implements the
//! supertype method — no bridge is needed, and it must compile + verify + dispatch correctly through the
//! supertype. Mission-core hit: `ChangeAwareEngine : ThrusterEngine` (~20 `override suspend fun`).
//! Needs the JVM toolchain + kotlin-stdlib + coroutines; skips otherwise.
use super::common;

fn run_box(src: &str, tag: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, tag, &[sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_member_overriding_nongeneric_interface() {
    if common::stdlib_jar().is_none()
        || common::coroutines_jar().is_none()
        || common::jdk_modules().is_none()
    {
        return;
    }
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        interface Engine { suspend fun run(x: Int): Int }\n\
        class Base : Engine { override suspend fun run(x: Int): Int = x * 10 }\n\
        class Decorator(val d: Engine) : Engine {\n\
            override suspend fun run(x: Int): Int = d.run(x) + 1\n\
        }\n\
        fun box(): String = runBlocking {\n\
            val dec: Engine = Decorator(Base())\n\
            val r = dec.run(5)\n\
            if (r == 51) \"OK\" else \"F: \" + r\n\
        }\n";
    assert_eq!(
        run_box(SRC, "Main").expect("suspend interface impl compile+run"),
        "OK"
    );
}

#[test]
fn suspend_override_needing_generic_bridge_is_skipped_not_miscompiled() {
    // A suspend member reached through a GENERIC ancestor (`A<T>`) via a raw-looking intermediate
    // (`B : A<String>`) DOES need an erasure bridge, which the coroutine lowering can't fix up. krusty
    // must SKIP the file (emit nothing runnable) rather than emit a broken bridge → `AbstractMethodError`.
    if common::stdlib_jar().is_none()
        || common::coroutines_jar().is_none()
        || common::jdk_modules().is_none()
    {
        return;
    }
    const SRC: &str = "interface A<T> { suspend fun f(x: T): T }\n\
        interface B : A<String>\n\
        class C : B { override suspend fun f(x: String): String = x }\n\
        fun box(): String = \"OK\"\n";
    assert!(
        run_box(SRC, "Main").is_none(),
        "a suspend override needing a generic bridge must be skipped, not miscompiled"
    );
}
