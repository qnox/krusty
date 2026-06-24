//! Anonymous-object capture (slice 1+2): an `object : I { … }` expression whose body reads an enclosing
//! function PARAMETER or a read-only LOCAL is rewritten so the captured name becomes a constructor `val`
//! property of the synth class (passed at construction) — kotlinc captures the same names into fields.
//! A WRITTEN `var` capture (needing a shared `Ref` cell) is NOT modeled and cleanly skips. Round-tripped
//! on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn captures_enclosing_parameter() {
    const SRC: &str = "interface S { fun g(): String }\n\
fun make(s: String): S = object : S { override fun g(): String = s }\n\
fun box(): String = make(\"OK\").g()\n";
    assert_eq!(run(SRC).expect("param capture compiles + runs"), "OK");
}

#[test]
fn captures_readonly_local_val() {
    const SRC: &str = "interface S { fun g(): String }\n\
fun box(): String {\n\
    val msg = \"OK\"\n\
    val s: S = object : S { override fun g(): String = msg }\n\
    return s.g()\n\
}\n";
    assert_eq!(run(SRC).expect("local val capture compiles + runs"), "OK");
}

#[test]
fn captures_int_local_in_computation() {
    const SRC: &str = "interface I { fun get(): Int }\n\
fun box(): String {\n\
    val n = 42\n\
    val i = object : I { override fun get() = n }\n\
    return if (i.get() == 42) \"OK\" else \"no\"\n\
}\n";
    assert_eq!(run(SRC).expect("int local capture compiles + runs"), "OK");
}

#[test]
fn written_var_capture_is_skipped_not_miscompiled() {
    // A captured `var` written inside the anon needs a shared `Ref` cell (not modeled) — krusty must
    // SKIP (compile error → None), never capture-by-value and lose the write.
    const SRC: &str = "interface R { fun run() }\n\
fun box(): String {\n\
    var acc = \"fail\"\n\
    val r = object : R { override fun run() { acc = \"OK\" } }\n\
    r.run(); return acc\n\
}\n";
    assert!(
        run(SRC).is_none(),
        "written-var capture must skip, not miscompile"
    );
}

#[test]
fn noncapturing_anon_still_works() {
    const SRC: &str = "interface I { fun f(): String }\n\
fun box(): String = (object : I { override fun f() = \"OK\" }).f()\n";
    assert_eq!(run(SRC).expect("non-capturing anon compiles + runs"), "OK");
}
