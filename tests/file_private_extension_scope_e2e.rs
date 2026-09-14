//! A file-private extension is a candidate only in its OWN file.
//!
//! Two sibling files in one package may each declare `private fun Ep.toInfo()`. The body checker
//! filters those by the calling file (`Checker::source_callable_visible`), but the SIGNATURE pass
//! did not — so a call in one of them saw a two-candidate family and reported
//!
//! ```text
//! error: none of the following candidates is applicable:
//! fun toInfo(): <not determined>
//! fun toInfo(): Info
//! ```
//!
//! `<not determined>` is the file's OWN declaration, whose inferred return is not solved at that
//! point; the sibling's explicit return is the second. Filtering the sibling out leaves one
//! candidate, and the ordinary same-file forward reference resolves it.
//!
//! A candidate's file comes from its declaration ANCHOR: `source_key` is unset for these, so
//! filtering on that alone changes nothing.

use super::common;

/// The failing shape: the call precedes its own file's declaration, and a sibling file declares the
/// same private extension with an explicit return.
#[test]
fn a_sibling_files_private_extension_is_not_a_candidate() {
    let sources = [
        (
            "A.kt",
            "package p\n\
             class Ep(val path: String)\n\
             class Info(val path: String)\n\
             private fun use(ep: Ep): String = ep.toInfo().path\n\
             private fun Ep.toInfo() = Info(path)\n\
             fun box(): String = if (use(Ep(\"ok\")) == \"ok\") \"OK\" else \"F\"\n",
        ),
        (
            "B.kt",
            "package p\n\
             private fun Ep.toInfo(): Info = Info(path + \"!\")\n\
             private fun other(ep: Ep): String = ep.toInfo().path\n",
        ),
    ];
    let result = common::compiler_diagnostics(&sources, &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "the reference compiler accepts two sibling file-private extensions"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must scope a file-private extension to its own file"
    );
    common::expect_box_ok_files_with_stdlib(&sources, "FilePrivateExt");
}

/// Each file reaches its OWN declaration, so the two bodies produce different values — proof the
/// filter selects per file rather than collapsing both to one declaration.
#[test]
fn each_file_reaches_its_own_private_extension() {
    let sources = [
        (
            "A.kt",
            "package p\n\
             class Ep(val path: String)\n\
             private fun Ep.mark() = path + \"-a\"\n\
             fun fromA(ep: Ep): String = ep.mark()\n",
        ),
        (
            "B.kt",
            "package p\n\
             private fun Ep.mark(): String = ep().path + \"-b\"\n\
             private fun Ep.ep(): Ep = this\n\
             fun fromB(ep: Ep): String = ep.mark()\n",
        ),
        (
            "C.kt",
            "package p\n\
             fun box(): String {\n\
             \x20   val ep = Ep(\"x\")\n\
             \x20   return if (fromA(ep) == \"x-a\" && fromB(ep) == \"x-b\") \"OK\"\n\
             \x20   else \"F:${fromA(ep)}/${fromB(ep)}\"\n\
             }\n",
        ),
    ];
    let result = common::compiler_diagnostics(&sources, &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "the reference compiler binds each file to its own private extension"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must bind each file to its own private extension"
    );
    common::expect_box_ok_files_with_stdlib(&sources, "FilePrivateOwn");
}
