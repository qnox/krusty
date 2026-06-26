//! A branchy value — a `?.let { … }` safe call (lowered to a null-check `when`, with the `let` body
//! spliced inline) — used as a property INITIALIZER stored through `this`. The receiver pushed for the
//! field store must NOT be left on the operand stack across the safe call's null branch: the emitter
//! evaluates the branchy value into a temp local first, then stores it (a side-effect-free `this`
//! receiver keeps Kotlin's left-to-right order). Otherwise the stackmap frame at the branch target omits
//! the ambient receiver → `VerifyError`. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn safe_call_lambda_field_initializer() {
    const SRC: &str = "class C(val s: String?) { val u: String? = s?.let { x -> x + \"!\" } }\n\
fun box(): String = C(\"a\").u ?: \"n\"\n";
    assert_eq!(
        run(SRC).expect("safe-call lambda field init compiles + runs"),
        "a!"
    );
}

#[test]
fn safe_call_lambda_field_initializer_null_branch() {
    // The null branch (receiver is null) must also merge correctly with the ambient receiver on the stack.
    const SRC: &str = "class C(val s: String?) { val u: String? = s?.let { x -> x + \"!\" } }\n\
fun box(): String = C(null).u ?: \"was-null\"\n";
    assert_eq!(
        run(SRC).expect("safe-call lambda field init (null) compiles + runs"),
        "was-null"
    );
}
