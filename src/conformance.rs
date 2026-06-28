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

/// Whether a box test applies to the backend tokens `names`, per kotlinc's test-runner directives, for
/// krusty's configuration: **Kotlin 2.4.0 (K2) frontend + JVM_IR backend**.
/// - `// TARGET_BACKEND:` restricts the test to the listed backends (absent = all).
/// - `// IGNORE_BACKEND:` mutes the test on the listed backends for ALL frontends → exclude.
/// - `// IGNORE_BACKEND_K2[_MULTI_MODULE]:` mutes it under the K2 frontend → exclude (krusty is K2).
/// - `// DONT_TARGET_EXACT_BACKEND:` the test doesn't target that backend → exclude.
/// - `// IGNORE_BACKEND_K1:` mutes it under the OLD K1 frontend ONLY → krusty is NOT K1, so this must
///   NOT exclude (excluding it under-counts: the test is valid for krusty's K2 semantics).
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
                || l.starts_with("// IGNORE_BACKEND_K2:")
                || l.starts_with("// IGNORE_BACKEND_K2_MULTI_MODULE:")
                || l.starts_with("// DONT_TARGET_EXACT_BACKEND:")
        })
        .all(|l| !mentions(l.split_once(':').map(|x| x.1).unwrap_or("").trim()))
}

/// Whether the test applies to krusty's backend (the common case of [`backend_applicable`]).
pub fn applies(src: &str) -> bool {
    backend_applicable(src, BACKENDS) && !needs_unmodeled_compiler_flag(src)
}

/// A directive requesting semantics krusty doesn't model. The test's expected `box()` outcome assumes
/// that option, so running it against krusty's default semantics is unsound.
pub fn needs_unmodeled_compiler_flag(src: &str) -> bool {
    const UNMODELED_FREE_ARGS: &[&str] = &["genericSafeCasts"];
    const UNMODELED_LANGUAGE_FLAGS: &[&str] = &["+UnrestrictedBuilderInference"];
    const UNMODELED_DIRECTIVES: &[&str] = &["KJS_WITH_FULL_RUNTIME"];
    const UNMODELED_SOURCE_MARKERS: &[&str] = &["ExperimentalTypeInference"];

    fn line_contains_any(src: &str, prefix: &str, needles: &[&str]) -> bool {
        src.lines()
            .filter(|l| l.starts_with(prefix))
            .any(|l| needles.iter().any(|needle| l.contains(needle)))
    }

    line_contains_any(src, "// FREE_COMPILER_ARGS:", UNMODELED_FREE_ARGS)
        || line_contains_any(src, "// LANGUAGE:", UNMODELED_LANGUAGE_FLAGS)
        || UNMODELED_DIRECTIVES.iter().any(|name| directive(src, name))
        || UNMODELED_SOURCE_MARKERS
            .iter()
            .any(|marker| src.contains(marker))
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
