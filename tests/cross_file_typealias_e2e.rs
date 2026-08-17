//! A source `typealias` referenced from ANOTHER file of the same module. A same-file use is
//! rewritten structurally by the parse seam, so the checker never sees it; a cross-file use
//! reaches the checker as the bare alias spelling and resolves through the scoped source-alias
//! channel (`scoped_source_alias_ty`): own package first, then imports — never module-wide.
//!
//! The motivating shape is intellij-community's `intellij.kotlin.base.projectModel`:
//! `typealias KotlinDependencyId = Long` in one file, `Array<KotlinDependencyId>` member
//! properties in siblings. Before the fix the checker silently produced `<error>` for them and
//! metadata emission panicked (`semantic type '<error>' cannot appear in Kotlin metadata`).

use super::common;

/// The distilled intellij shape: a primitive-target alias used by a sibling file's member
/// property, inside a generic argument. Pre-fix: metadata invariant panic.
#[test]
fn cross_file_primitive_alias_in_member_property_compiles() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[
            ("Component", "package app\ntypealias Id = Long\n"),
            (
                "SourceSet",
                "package app\ninterface SourceSet { val deps: Array<Id> }\n",
            ),
        ],
        std::slice::from_ref(&stdlib),
        Some(&jdk),
    );
    assert!(
        classes.is_some_and(|classes| !classes.is_empty()),
        "a same-package cross-file primitive typealias must resolve and compile"
    );
}

/// A cross-file primitive alias as a top-level function parameter erases to the alias target's
/// primitive descriptor, exactly like the same-file spelling (kotlinc: `f(J)V`).
#[test]
fn cross_file_primitive_alias_erases_to_primitive() {
    let out = common::compile_and_run_files_with_stdlib(&[
        ("Alias.kt", "package app\ntypealias Id = Long\n"),
        (
            "Main.kt",
            "package app\n\
             fun f(x: Id): Long = x + 1L\n\
             fun box(): String = if (f(41L) == 42L) \"OK\" else \"fail\"\n",
        ),
    ]);
    if let Some(out) = out {
        assert_eq!(out, "OK");
    }
}

/// A generic alias (`typealias P<T> = Map<T, Long>`) substitutes the use-site arguments into the
/// collected expansion when referenced cross-file.
#[test]
fn cross_file_generic_alias_substitutes_arguments() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[
            ("Alias", "package app\ntypealias P<T> = Map<T, Long>\n"),
            (
                "Use",
                "package app\ninterface Holder { val m: P<String> }\n",
            ),
        ],
        std::slice::from_ref(&stdlib),
        Some(&jdk),
    );
    assert!(
        classes.is_some_and(|classes| !classes.is_empty()),
        "a same-package cross-file generic typealias must resolve and compile"
    );
}

/// A use-site projection through a cross-file generic alias reaches the emitted JVM generic
/// signature: `P<out CharSequence>` keeps its `+` marker (byte-identical to kotlinc's
/// `()Ljava/util/Map<+Ljava/lang/CharSequence;Ljava/lang/Long;>;`), and `P<*>` keeps krusty's
/// star form — the out-projected upper bound `+Ljava/lang/Object;`, identical to the SAME-FILE
/// spelling of this alias (kotlinc renders an unbounded star as `*`; that channel-wide rendering
/// divergence predates this alias work and is consistent across spellings). Before the projection
/// mapping, the cross-file channel dropped both markers (invariant `Ljava/lang/Object;` /
/// `Ljava/lang/CharSequence;`), so the same alias produced different signatures same-file vs
/// cross-file.
#[test]
fn cross_file_alias_projection_reaches_generic_signature() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[
            ("Alias", "package app\ntypealias P<T> = Map<T, Long>\n"),
            (
                "Use",
                "package app\n\
                 interface Holder {\n\
                 \x20   val m: P<*>\n\
                 \x20   val n: P<out CharSequence>\n\
                 }\n",
            ),
        ],
        std::slice::from_ref(&stdlib),
        Some(&jdk),
    )
    .expect("the projected cross-file alias must compile");
    let holder = classes
        .iter()
        .find(|(name, _)| name.ends_with("Holder"))
        .map(|(_, bytes)| krusty::jvm::classreader::parse_class(bytes).expect("parse Holder"))
        .expect("Holder class emitted");
    let signature = |method: &str| {
        holder
            .methods
            .iter()
            .find(|m| m.name == method)
            .and_then(|m| m.signature.clone())
    };
    assert_eq!(
        signature("getM").as_deref(),
        Some("()Ljava/util/Map<+Ljava/lang/Object;Ljava/lang/Long;>;"),
        "star projection must survive the cross-file alias substitution"
    );
    assert_eq!(
        signature("getN").as_deref(),
        Some("()Ljava/util/Map<+Ljava/lang/CharSequence;Ljava/lang/Long;>;"),
        "out projection must survive the cross-file alias substitution"
    );
}

/// The cross-file projected forms above must be IDENTICAL to the same-file spelling of the same
/// alias — one alias, one signature, whichever file spells it.
#[test]
fn cross_file_alias_projection_matches_same_file_spelling() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let source = "package app\n\
                  typealias P<T> = Map<T, Long>\n\
                  interface Holder {\n\
                  \x20   val m: P<*>\n\
                  \x20   val n: P<out CharSequence>\n\
                  }\n";
    let classes = common::compile_in_process_files(
        &[("SameFile", source)],
        std::slice::from_ref(&stdlib),
        Some(&jdk),
    )
    .expect("the same-file projected alias must compile");
    let holder = classes
        .iter()
        .find(|(name, _)| name.ends_with("Holder"))
        .map(|(_, bytes)| krusty::jvm::classreader::parse_class(bytes).expect("parse Holder"))
        .expect("Holder class emitted");
    let signature = |method: &str| {
        holder
            .methods
            .iter()
            .find(|m| m.name == method)
            .and_then(|m| m.signature.clone())
    };
    assert_eq!(
        signature("getM").as_deref(),
        Some("()Ljava/util/Map<+Ljava/lang/Object;Ljava/lang/Long;>;")
    );
    assert_eq!(
        signature("getN").as_deref(),
        Some("()Ljava/util/Map<+Ljava/lang/CharSequence;Ljava/lang/Long;>;")
    );
}

/// The scoped channel must NOT resolve an alias from an unimported foreign package (that would
/// reintroduce module-wide simple-name resolution — kotlinc rejects it).
#[test]
fn cross_package_alias_without_import_is_diagnosed() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Alias.kt", "package other\ntypealias Id = Long\n"),
        ("Use.kt", "package app\nfun f(x: Id) {}\n"),
    ]) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unresolved reference 'Id'")),
        "expected an unresolved-reference diagnostic, got: {diags:?}"
    );
}

/// An explicit import brings a foreign-package alias into scope (control for the scoping rule).
#[test]
fn cross_package_alias_with_import_resolves() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Alias.kt", "package other\ntypealias Id = Long\n"),
        ("Use.kt", "package app\nimport other.Id\nfun f(x: Id) {}\n"),
    ]) else {
        return;
    };
    assert!(
        diags.is_empty(),
        "an imported cross-package typealias must resolve, got: {diags:?}"
    );
}

/// Wrong arity against the alias declaration is the alias arity error, not a bogus
/// unresolved-reference report.
#[test]
fn cross_file_alias_wrong_arity_reports_arity() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Alias.kt", "package app\ntypealias P<T> = Map<T, Long>\n"),
        (
            "Use.kt",
            "package app\ninterface Holder { val m: P<String, Int> }\n",
        ),
    ]) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|d| d.contains("wrong number of type arguments for type alias 'P'")),
        "expected the alias arity diagnostic, got: {diags:?}"
    );
}
