//! An explicit (non-star) import outranks a same-package declaration of the same simple name.
//!
//! Kotlin's classifier scope tower places explicit imports ABOVE the current package, which is
//! above star imports. krusty consulted the same-package declaration first, so a file that
//! imported `other.Payload` while a sibling file in its OWN package also declared `Payload` bound
//! every mention to the sibling. The visible symptom is not "unresolved" but a wrong constructor:
//! named arguments that exist only on the imported class report "no parameter with name 'x'".

use super::common;

/// The tower order itself: the imported class supplies the constructor, so its parameter names
/// must be the ones that bind.
#[test]
fn explicit_import_wins_over_same_package_class() {
    let sources = [
        (
            "Payload.kt",
            "package app.dto\ndata class Payload(val alpha: String, val beta: String)\n",
        ),
        (
            "Local.kt",
            "package app.ctl\ndata class Payload(val gamma: String)\n",
        ),
        (
            "Use.kt",
            "package app.ctl\n\
             import app.dto.Payload\n\
             fun make(): Payload = Payload(alpha = \"a\", beta = \"b\")\n",
        ),
    ];
    let result = common::compiler_diagnostics(&sources, &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the explicit-import precedence fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source set kotlinc accepts"
    );
}

/// Control: WITHOUT the import the same-package declaration still wins, so the fix may not simply
/// invert the two rungs.
#[test]
fn same_package_class_still_wins_without_an_import() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        (
            "Payload.kt",
            "package app.dto\ndata class Payload(val alpha: String, val beta: String)\n",
        ),
        (
            "Local.kt",
            "package app.ctl\ndata class Payload(val gamma: String)\n",
        ),
        (
            "Use.kt",
            "package app.ctl\nfun make(): Payload = Payload(gamma = \"g\")\n",
        ),
    ]) else {
        return;
    };
    assert!(
        diags.is_empty(),
        "the same-package declaration must still bind when nothing is imported, got: {diags:?}"
    );
}

/// Control: a STAR import ranks BELOW the current package, so the same-package declaration keeps
/// winning against `import app.dto.*`.
#[test]
fn same_package_class_outranks_a_star_import() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        (
            "Payload.kt",
            "package app.dto\ndata class Payload(val alpha: String, val beta: String)\n",
        ),
        (
            "Local.kt",
            "package app.ctl\ndata class Payload(val gamma: String)\n",
        ),
        (
            "Use.kt",
            "package app.ctl\n\
             import app.dto.*\n\
             fun make(): Payload = Payload(gamma = \"g\")\n",
        ),
    ]) else {
        return;
    };
    assert!(
        diags.is_empty(),
        "a star import must not outrank the current package, got: {diags:?}"
    );
}

/// The full-compile contract: the imported class is the one actually constructed, so the module
/// emits classes rather than failing on the sibling's parameter names.
#[test]
fn explicit_import_shadowing_compiles() {
    common::expect_box_ok_files_with_stdlib(
        &[
            (
                "Payload.kt",
                "package app.dto\ndata class Payload(val alpha: String, val beta: String)\n",
            ),
            (
                "Local.kt",
                "package app.ctl\ndata class Payload(val gamma: String)\n",
            ),
            (
                "Use.kt",
                "package app.ctl\n\
                 import app.dto.Payload\n\
                 fun make(): Payload = Payload(alpha = \"a\", beta = \"b\")\n\
                 fun box(): String = if (make().alpha == \"a\") \"OK\" else \"wrong class\"\n",
            ),
        ],
        "ExplicitImportShadowsSamePackage",
    );
}
