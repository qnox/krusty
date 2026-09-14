//! A file-private extension is a candidate only in its own source file.

use super::common;

/// The failing signature shape: the call precedes its own file's declaration, while a sibling file
/// declares the same private extension with an already-known return type.
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

/// Each file reaches its own declaration. The different return values prove the provider selects by
/// source file rather than collapsing the declarations to one candidate.
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

/// A companion extension is looked up through the associated classifier for `C::mark`. That
/// normalization must not bypass the declaring file's visibility.
#[test]
fn a_companion_extension_callable_reference_selects_its_own_files_declaration() {
    let sources = [
        (
            "A.kt",
            "// LANGUAGE: +CompanionBlocksAndExtensions\n\
             package p\n\
             class C\n\
             private companion fun C.mark(): String = \"A\"\n\
             fun fromA(): String {\n\
             \x20   val reference: () -> String = C::mark\n\
             \x20   return reference()\n\
             }\n\
             fun box(): String = if (fromA() + fromB() == \"AB\") \"OK\" else \"FAIL\"\n",
        ),
        (
            "B.kt",
            "// LANGUAGE: +CompanionBlocksAndExtensions\n\
             package p\n\
             private companion fun C.mark(): String = \"B\"\n\
             fun fromB(): String {\n\
             \x20   val reference: () -> String = C::mark\n\
             \x20   return reference()\n\
             }\n",
        ),
    ];
    let result = common::compiler_diagnostics_with_reference_args(
        &sources,
        &[common::stdlib_jar()],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            0,
            "warning: ATTENTION!\n\
             This build uses unsafe internal compiler arguments:\n\
             \n\
             -XXLanguage:+CompanionBlocksAndExtensions\n\
             \n\
             This mode is not recommended for production use,\n\
             as no stability/compatibility guarantees are given on\n\
             compiler or generated code. Use it at your own risk!\n\
             \n\
             warning: following manually enabled features will force generation of pre-release binaries: CompanionBlocksAndExtensions\n",
        ),
        "the reference compiler scopes companion extensions before callable-reference selection"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must not expose a sibling file's private companion extension through C::mark"
    );
    common::expect_box_ok_files_with_stdlib(&sources, "FilePrivateCompanionRef");
}
