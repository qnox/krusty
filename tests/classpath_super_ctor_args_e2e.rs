//! `class E(m: String) : RuntimeException(m)` — extending a CLASSPATH base with a super-constructor
//! ARGUMENT. krusty only emitted a no-arg `super()` to a classpath base; a parameterized one bailed
//! ("this construct is not yet supported by the IR backend"). It now resolves the classpath base
//! constructor matching the args and emits `super(args)`.

use super::common;

fn compile(src: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_in_process(src, "File", &[sl, jdk.clone()], Some(&jdk))
}

#[test]
fn extends_runtime_exception_with_message() {
    let classes = compile(
        "package demo\n\
         class EntityNotFoundException(message: String) : RuntimeException(message)\n",
    )
    .expect("krusty failed to compile an exception subclass");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "demo/EntityNotFoundException")
        .expect("class emitted");
    let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    // The `<init>` calls the classpath base's String constructor.
    assert!(has("java/lang/RuntimeException"), "base not referenced");
    assert!(
        has("(Ljava/lang/String;)V"),
        "super(String) descriptor missing"
    );
}
