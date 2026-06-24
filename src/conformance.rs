//! Shared codegen/box conformance-test directives — the SINGLE source of truth for which tests apply
//! to krusty's JVM backend and which extra libraries their classpath needs. Used by BOTH the
//! conformance gate (`tests/kotlin_box_ir_jvm_conformance.rs`) and the `survey` bin, so their
//! eligibility decisions and classpaths never drift (which previously let survey over-count by
//! compiling against libraries a test didn't ask for, and let backend-excluded tests slip through).

/// Backend tokens krusty identifies as — it emits the JVM (IR) backend's bytecode.
pub const BACKENDS: &[&str] = &["JVM", "JVM_IR"];

/// Whether the source declares the line directive `// <name>` (e.g. `WITH_REFLECT`). Matches the first
/// `//`-comment whose first token (split on space/colon) is exactly `name`.
pub fn directive(src: &str, name: &str) -> bool {
    src.lines().any(|l| {
        let l = l.trim();
        l.starts_with("//")
            && l.trim_start_matches('/')
                .trim_start()
                .split([' ', ':'])
                .next()
                == Some(name)
    })
}

/// Whether a box test applies to the backend tokens `names`, per kotlinc's directives:
/// `// TARGET_BACKEND:` restricts the test to the listed backends (absent = all); `// IGNORE_BACKEND
/// [_K1|_K2]:` and `// DONT_TARGET_EXACT_BACKEND:` exclude the listed backends. kotlinc's own runner
/// skips a `DONT_TARGET_EXACT_BACKEND: JVM_IR` test for the JVM_IR backend krusty emits, so it must not
/// count against the gate.
pub fn backend_applicable(src: &str, names: &[&str]) -> bool {
    let mentions = |line: &str| line.split(',').any(|t| names.contains(&t.trim()));
    if let Some(l) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
        if !mentions(l.trim_start_matches("// TARGET_BACKEND:").trim()) {
            return false;
        }
    }
    src.lines()
        .filter(|l| {
            l.starts_with("// IGNORE_BACKEND:")
                || l.starts_with("// IGNORE_BACKEND_K1:")
                || l.starts_with("// IGNORE_BACKEND_K2:")
                || l.starts_with("// DONT_TARGET_EXACT_BACKEND:")
        })
        .all(|l| !mentions(l.split_once(':').map(|x| x.1).unwrap_or("").trim()))
}

/// Whether the test applies to krusty's backend (the common case of [`backend_applicable`]).
pub fn applies(src: &str) -> bool {
    backend_applicable(src, BACKENDS)
}

/// The EXTRA libraries (beyond kotlin-stdlib, which `kotlinc` always supplies) a test's classpath needs,
/// per its directives. Both the gate and the survey select the same jars from this so a test never
/// compiles against a library it didn't request.
#[derive(Clone, Copy, Default, Debug)]
pub struct ExtraLibs {
    pub reflect: bool,
    pub stdlib_jdk8: bool,
    pub coroutines: bool,
}

pub fn extra_libs(src: &str) -> ExtraLibs {
    ExtraLibs {
        reflect: directive(src, "WITH_REFLECT"),
        stdlib_jdk8: directive(src, "STDLIB_JDK8"),
        coroutines: directive(src, "WITH_COROUTINES"),
    }
}
