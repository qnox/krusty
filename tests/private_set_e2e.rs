//! A property with a visibility-only setter (`var x = 0; private set`) is a plain backing-field
//! property whose setter is emitted `private`. Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn private_setter() {
    const SRC: &str = "class P {\n\
    var x: Int = 0\n\
        private set\n\
    fun bump() { this.x = this.x + 1 }\n\
}\n\
fun box(): String { val p = P(); p.bump(); p.bump(); return if (p.x == 2) \"OK\" else \"fail\" }\n";
    assert_eq!(run(SRC).expect("private set compiles + runs"), "OK");
}

#[test]
fn private_setter_is_not_writable_outside_its_owner() {
    // Moving default-accessor synthesis into a backend must not erase declaration visibility. This is a
    // frontend assertion as well as an ABI invariant: accepting the assignment would make the backend
    // synthesize a public setter for source that explicitly declared it private.
    let diagnostics = common::front_end_diagnostics(
        "class P { var x: Int = 0; private set }\n\
         fun misuse(p: P) { p.x = 1 }\n",
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("private") && message.contains("x")),
        "expected private-setter diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn synthesized_private_setter_keeps_private_jvm_access() {
    // A diagnostic alone does not protect the emitted ABI: after default accessor synthesis moved from
    // common lowering into the JVM backend, the compiler could reject outside writes yet still expose a
    // public `setX`. Inspect the emitted method flags so declaration visibility is tested at both layers.
    let classes = common::compile_in_process(
        "class PrivateCounter { var x: Int = 0; private set }\n",
        "PrivateCounter",
        &[],
        None,
    )
    .expect("private-setter class should emit");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "PrivateCounter")
        .expect("PrivateCounter.class");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse PrivateCounter.class");
    let setter = class.method("setX", "(I)V").expect("synthesized setX");
    assert_ne!(
        setter.access & 0x0002,
        0,
        "private set must synthesize ACC_PRIVATE"
    );
    assert_eq!(
        setter.access & 0x0001,
        0,
        "private set must not synthesize ACC_PUBLIC"
    );
}
