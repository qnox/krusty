//! A class with a `suspend` member that overrides a NON-GENERIC supertype method (an interface impl /
//! suspend decorator). No generic erasure is involved, so the CPS override directly implements the
//! supertype method — no bridge is needed, and it must compile + verify + dispatch correctly through the
//! supertype. Production hit: an engine class implementing a ~20-method suspend interface.
//! Requires the JVM toolchain + kotlin-stdlib + coroutines; missing provisioning is a test failure.
use super::common;

fn run_box(src: &str, tag: &str) -> Option<String> {
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(src, tag, &[sl, coro, jdk.clone()], Some(jdk.as_path()))
}

#[test]
fn suspend_member_overriding_nongeneric_interface() {
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
fn suspend_override_needing_generic_bridge_runs() {
    // Dispatch through the erased generic ancestor, not through `C`, so this observes the bridge
    // descriptor and its CPS continuation/result adaptation rather than merely compiling it.
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        interface A<T> { suspend fun f(x: T): T }\n\
        interface B : A<String>\n\
        class C : B { override suspend fun f(x: String): String = x }\n\
        fun box(): String = runBlocking {\n\
            val erased: A<String> = C()\n\
            erased.f(\"OK\")\n\
        }\n";
    assert_eq!(
        run_box(SRC, "Main").expect("generic suspend bridge compile+run"),
        "OK"
    );
}
