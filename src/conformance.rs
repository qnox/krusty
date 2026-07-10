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
    const UNMODELED_LANGUAGE_FLAGS: &[&str] = &[
        "+UnrestrictedBuilderInference",
        // A value class with a secondary constructor that has a body — krusty models value classes
        // only as plain constructions, so this feature's expected `box()` is unsound to run.
        "+ValueClassesSecondaryConstructorWithBody",
    ];
    // `PROPERTY_LAZY_INITIALIZATION` makes file/top-level properties initialize LAZILY (on first
    // access) with getter-triggered ordering; krusty initializes them eagerly in `<clinit>`, so a test
    // whose expected `box()` depends on the lazy order/accessor shape is unsound to run.
    const UNMODELED_DIRECTIVES: &[&str] =
        &["KJS_WITH_FULL_RUNTIME", "PROPERTY_LAZY_INITIALIZATION"];
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

/// One `// MODULE:` block from a Kotlin box test: its name, regular classpath dependency modules,
/// and the source files that belong to the module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBlock {
    pub name: String,
    pub deps: Vec<String>,
    pub files: Vec<(String, String)>,
}

/// Split a `// MODULE:`-partitioned box test into modules, each with its `// FILE:` blocks. Returns
/// `None` for shapes krusty's module model does not cover, so the conformance harness can skip them
/// instead of mis-grading them.
pub fn split_modules(src: &str) -> Option<Vec<ModuleBlock>> {
    let mut mods: Vec<ModuleBlock> = Vec::new();
    let mut cur_file: Option<String> = None;
    let mut cur = String::new();
    let flush = |mods: &mut Vec<ModuleBlock>, cur_file: &mut Option<String>, cur: &mut String| {
        if let Some(fname) = cur_file.take() {
            if let Some(m) = mods.last_mut() {
                m.files.push((fname, std::mem::take(cur)));
            }
        }
    };
    for line in src.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("// MODULE:") {
            flush(&mut mods, &mut cur_file, &mut cur);
            let header = rest.trim();
            if header.matches('(').count() > 1 {
                return None;
            }
            let name_end = header.find('(').unwrap_or(header.len());
            let name = header[..name_end].trim().to_string();
            let deps = header[name_end..]
                .trim_start_matches('(')
                .split(')')
                .next()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            mods.push(ModuleBlock {
                name,
                deps,
                files: Vec::new(),
            });
        } else if let Some(rest) = t.strip_prefix("// FILE:") {
            flush(&mut mods, &mut cur_file, &mut cur);
            let fname = rest.trim();
            if !fname.ends_with(".kt") {
                return None;
            }
            let stem = fname
                .strip_suffix(".kt")
                .unwrap_or(fname)
                .rsplit('/')
                .next()
                .unwrap_or(fname)
                .to_string();
            cur_file = Some(stem);
        } else if cur_file.is_some() {
            cur.push_str(line);
            cur.push('\n');
        } else if !mods.is_empty() && !t.is_empty() && !t.starts_with("//") {
            cur_file = Some(mods.last().unwrap().name.clone());
            cur.push_str(line);
            cur.push('\n');
        }
    }
    flush(&mut mods, &mut cur_file, &mut cur);
    if mods.len() < 2 || mods.iter().any(|m| m.files.is_empty()) {
        return None;
    }
    Some(mods)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_present_when_first_token_matches() {
        assert!(directive(
            "// WITH_REFLECT\nfun box() = \"OK\"",
            "WITH_REFLECT"
        ));
    }

    #[test]
    fn directive_matches_with_colon_separator() {
        // The token is split on space OR colon, so a `NAME: value` directive matches `NAME`.
        assert!(directive("// TARGET_BACKEND: JVM_IR", "TARGET_BACKEND"));
    }

    #[test]
    fn directive_matches_with_space_separator() {
        assert!(directive("// WITH_REFLECT extra", "WITH_REFLECT"));
    }

    #[test]
    fn directive_absent_returns_false() {
        assert!(!directive("// WITH_COROUTINES\n", "WITH_REFLECT"));
    }

    #[test]
    fn directive_requires_exact_first_token_not_substring() {
        // `WITH_REFLECT_EXTRA` must NOT satisfy a query for `WITH_REFLECT`.
        assert!(!directive("// WITH_REFLECT_EXTRA", "WITH_REFLECT"));
    }

    #[test]
    fn directive_ignores_non_comment_lines() {
        assert!(!directive("val WITH_REFLECT = 1", "WITH_REFLECT"));
    }

    #[test]
    fn directive_tolerates_leading_whitespace_and_extra_slashes() {
        assert!(directive("    /// WITH_REFLECT", "WITH_REFLECT"));
        assert!(directive("\t// WITH_REFLECT", "WITH_REFLECT"));
    }

    #[test]
    fn directive_finds_among_multiple_lines() {
        let src = "// FILE: a.kt\n// WITH_COROUTINES\nfun box() = \"OK\"";
        assert!(directive(src, "WITH_COROUTINES"));
        assert!(directive(src, "FILE"));
    }

    #[test]
    fn directive_empty_source_is_false() {
        assert!(!directive("", "WITH_REFLECT"));
    }

    #[test]
    fn backend_applicable_no_directives_applies() {
        assert!(backend_applicable("fun box() = \"OK\"", BACKENDS));
    }

    #[test]
    fn backend_applicable_target_backend_matching_included() {
        assert!(backend_applicable("// TARGET_BACKEND: JVM_IR", BACKENDS));
        assert!(backend_applicable("// TARGET_BACKEND: JVM", BACKENDS));
    }

    #[test]
    fn backend_applicable_target_backend_nonmatching_excluded() {
        assert!(!backend_applicable("// TARGET_BACKEND: JS", BACKENDS));
        assert!(!backend_applicable("// TARGET_BACKEND: NATIVE", BACKENDS));
    }

    #[test]
    fn backend_applicable_target_backend_comma_list() {
        assert!(backend_applicable(
            "// TARGET_BACKEND: JS, JVM_IR",
            BACKENDS
        ));
    }

    #[test]
    fn backend_applicable_ignore_backend_excludes() {
        assert!(!backend_applicable("// IGNORE_BACKEND: JVM_IR", BACKENDS));
        assert!(!backend_applicable("// IGNORE_BACKEND: JVM", BACKENDS));
    }

    #[test]
    fn backend_applicable_ignore_backend_other_is_kept() {
        assert!(backend_applicable("// IGNORE_BACKEND: JS", BACKENDS));
    }

    #[test]
    fn backend_applicable_ignore_backend_k2_excludes() {
        // krusty is K2, so a K2 mute excludes.
        assert!(!backend_applicable(
            "// IGNORE_BACKEND_K2: JVM_IR",
            BACKENDS
        ));
        assert!(!backend_applicable(
            "// IGNORE_BACKEND_K2_MULTI_MODULE: JVM_IR",
            BACKENDS
        ));
    }

    #[test]
    fn backend_applicable_dont_target_exact_backend_excludes() {
        assert!(!backend_applicable(
            "// DONT_TARGET_EXACT_BACKEND: JVM_IR",
            BACKENDS
        ));
    }

    #[test]
    fn backend_applicable_ignore_backend_k1_is_not_excluded() {
        // krusty is NOT K1: a K1-only mute must NOT exclude (it isn't in the filtered set).
        assert!(backend_applicable("// IGNORE_BACKEND_K1: JVM_IR", BACKENDS));
    }

    #[test]
    fn backend_applicable_ignore_backend_comma_list() {
        assert!(!backend_applicable(
            "// IGNORE_BACKEND: JS, JVM_IR",
            BACKENDS
        ));
        assert!(backend_applicable(
            "// IGNORE_BACKEND: JS, NATIVE",
            BACKENDS
        ));
    }

    #[test]
    fn applies_combines_backend_and_flag_checks() {
        assert!(applies("fun box() = \"OK\""));
        // Backend-excluded → not applicable.
        assert!(!applies("// IGNORE_BACKEND: JVM_IR"));
        // Unmodeled flag → not applicable even though the backend is fine.
        assert!(!applies("// LANGUAGE: +UnrestrictedBuilderInference"));
    }

    #[test]
    fn needs_unmodeled_flag_free_compiler_args() {
        assert!(needs_unmodeled_compiler_flag(
            "// FREE_COMPILER_ARGS: -XXLanguage:+genericSafeCasts"
        ));
        assert!(!needs_unmodeled_compiler_flag(
            "// FREE_COMPILER_ARGS: -Xfoo"
        ));
    }

    #[test]
    fn needs_unmodeled_flag_language() {
        assert!(needs_unmodeled_compiler_flag(
            "// LANGUAGE: +UnrestrictedBuilderInference"
        ));
        assert!(!needs_unmodeled_compiler_flag(
            "// LANGUAGE: +SomethingElse"
        ));
    }

    #[test]
    fn needs_unmodeled_flag_directive() {
        assert!(needs_unmodeled_compiler_flag("// KJS_WITH_FULL_RUNTIME"));
    }

    #[test]
    fn needs_unmodeled_flag_source_marker() {
        // Matched anywhere in the source, not just in a directive comment.
        assert!(needs_unmodeled_compiler_flag(
            "@OptIn(ExperimentalTypeInference::class)"
        ));
    }

    #[test]
    fn needs_unmodeled_flag_absent() {
        assert!(!needs_unmodeled_compiler_flag("fun box() = \"OK\""));
    }

    #[test]
    fn extra_libs_none_by_default() {
        let libs = extra_libs("fun box() = \"OK\"");
        assert!(!libs.reflect);
        assert!(!libs.stdlib_jdk8);
        assert!(!libs.coroutines);
    }

    #[test]
    fn extra_libs_reads_each_directive() {
        let libs = extra_libs("// WITH_REFLECT");
        assert!(libs.reflect);
        assert!(!libs.coroutines);

        let libs = extra_libs("// WITH_COROUTINES");
        assert!(libs.coroutines);
        assert!(!libs.reflect);

        let libs = extra_libs("// STDLIB_JDK8");
        assert!(libs.stdlib_jdk8);
    }

    #[test]
    fn extra_libs_reads_multiple_directives_together() {
        let src = "// WITH_REFLECT\n// WITH_COROUTINES\n// STDLIB_JDK8\nfun box() = \"OK\"";
        let libs = extra_libs(src);
        assert!(libs.reflect);
        assert!(libs.coroutines);
        assert!(libs.stdlib_jdk8);
    }

    #[test]
    fn backends_constant_lists_jvm_variants() {
        assert!(BACKENDS.contains(&"JVM"));
        assert!(BACKENDS.contains(&"JVM_IR"));
    }

    #[test]
    fn split_modules_two_modules_with_explicit_files_parse_names_deps_and_bodies() {
        let src = "\
// MODULE: lib
// FILE: lib.kt
class A
// MODULE: main(lib)
// FILE: main.kt
fun box(): String = \"OK\"
";
        let mods = split_modules(src).expect("two-module test should split");
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].name, "lib");
        assert!(mods[0].deps.is_empty());
        assert_eq!(
            mods[0].files,
            vec![("lib".to_string(), "class A\n".to_string())]
        );
        assert_eq!(mods[1].name, "main");
        assert_eq!(mods[1].deps, vec!["lib".to_string()]);
        assert_eq!(
            mods[1].files,
            vec![(
                "main".to_string(),
                "fun box(): String = \"OK\"\n".to_string()
            )]
        );
    }

    #[test]
    fn split_modules_multiple_regular_deps_split_on_comma() {
        let src = "\
// MODULE: a
// FILE: a.kt
class A
// MODULE: b
// FILE: b.kt
class B
// MODULE: main(a, b)
// FILE: main.kt
fun box(): String = \"OK\"
";
        let mods = split_modules(src).expect("should split");
        assert_eq!(mods.len(), 3);
        assert_eq!(mods[2].deps, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn split_modules_implicit_file_body_after_module_header() {
        let src = "\
// MODULE: lib
class A
// MODULE: main(lib)
fun box(): String = \"OK\"
";
        let mods = split_modules(src).expect("implicit-file test should split");
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].files[0].0, "lib");
        assert_eq!(mods[1].files[0].0, "main");
    }

    #[test]
    fn split_modules_nested_file_path_uses_leaf_stem() {
        let src = "\
// MODULE: lib
// FILE: pkg/Foo.kt
package pkg
class Foo
// MODULE: main(lib)
// FILE: main.kt
fun box(): String = \"OK\"
";
        let mods = split_modules(src).expect("should split");
        assert_eq!(mods[0].files[0].0, "Foo");
    }

    #[test]
    fn split_modules_friend_or_dependson_header_is_declined() {
        let src = "\
// MODULE: common
// FILE: common.kt
expect fun f(): String
// MODULE: main()()(common)
// FILE: main.kt
actual fun f(): String = \"OK\"
";
        assert!(split_modules(src).is_none());
    }

    #[test]
    fn split_modules_java_file_source_is_declined() {
        let src = "\
// MODULE: lib
// FILE: J.java
public class J {}
// MODULE: main(lib)
// FILE: main.kt
fun box(): String = \"OK\"
";
        assert!(split_modules(src).is_none());
    }

    #[test]
    fn split_modules_single_module_is_declined() {
        let src = "\
// MODULE: only
// FILE: only.kt
fun box(): String = \"OK\"
";
        assert!(split_modules(src).is_none());
    }

    #[test]
    fn split_modules_module_without_any_source_is_declined() {
        let src = "\
// MODULE: lib
// MODULE: main(lib)
// FILE: main.kt
fun box(): String = \"OK\"
";
        assert!(split_modules(src).is_none());
    }
}
