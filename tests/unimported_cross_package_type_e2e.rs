//! A declared type naming a class from ANOTHER package of the same module, without an import, is
//! "unresolved reference" (kotlinc parity) — never a silent acceptance and never an internal panic.
//!
//! The invariant this pins: the checker may not hand lowering/metadata a declaration whose checked
//! type contains `<error>` without a user-visible diagnostic. Before the fix, signature collection
//! resolved the bare simple name module-wide (ignoring packages) while the properly scoped checker
//! produced `Ty::Error` silently; the mismatch surfaced as
//! `invalid emitted metadata type: semantic type '<error>' cannot appear in Kotlin metadata`
//! (src/metadata/builder.rs's invariant detector) on intellij-community's
//! `intellij.kotlin.base.projectModel` module, with zero diagnostics.

use super::common;

/// The distilled intellij shape: an interface member property whose declared type carries the
/// unimported cross-package class as a GENERIC ARGUMENT. Pre-fix this panicked in metadata
/// emission; the contract is a plain checker error.
#[test]
fn member_property_generic_arg_from_unimported_package_is_diagnosed() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Platform.kt", "package other\nclass KotlinPlatform\n"),
        (
            "Container.kt",
            "interface KotlinPlatformContainer {\n\
             \x20   val platforms: Collection<KotlinPlatform>\n\
             }\n",
        ),
    ]) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unresolved reference 'KotlinPlatform'")),
        "expected an unresolved-reference diagnostic, got: {diags:?}"
    );
}

/// A primary-constructor property typed directly by the unimported cross-package class. Pre-fix
/// this was the other panicking shape (constructor value-parameter metadata).
#[test]
fn constructor_property_type_from_unimported_package_is_diagnosed() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Foo.kt", "package other\nclass Foo\n"),
        ("Holder.kt", "class Holder(val x: Foo)\n"),
    ]) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unresolved reference 'Foo'")),
        "expected an unresolved-reference diagnostic, got: {diags:?}"
    );
}

/// A top-level function signature naming the unimported class. Pre-fix this shape did not panic —
/// it silently COMPILED against the wrong-scope resolution (kotlinc rejects it), which is the same
/// invariant break without a detector.
#[test]
fn top_level_function_param_type_from_unimported_package_is_diagnosed() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Foo.kt", "package other\nclass Foo\n"),
        ("Take.kt", "fun take(x: Foo) {}\n"),
    ]) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unresolved reference 'Foo'")),
        "expected an unresolved-reference diagnostic, got: {diags:?}"
    );
}

/// A top-level property annotated with the unimported class — the `check_property` channel rather
/// than the shared declaration-type channel.
#[test]
fn top_level_property_type_from_unimported_package_is_diagnosed() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Foo.kt", "package other\nclass Foo\n"),
        ("Prop.kt", "val p: Foo? = null\n"),
    ]) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unresolved reference 'Foo'")),
        "expected an unresolved-reference diagnostic, got: {diags:?}"
    );
}

/// The full-compile contract for the panicking shape: the module compile produces NO artifacts
/// (errors gate emission, exactly like any other checker error) — and, pre-fix, this is the test
/// that hit the metadata invariant panic instead of returning cleanly.
#[test]
fn unimported_cross_package_type_compile_yields_no_artifacts() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[
            ("Platform", "package other\nclass KotlinPlatform\n"),
            (
                "Container",
                "interface KotlinPlatformContainer {\n\
                 \x20   val platforms: Collection<KotlinPlatform>\n\
                 }\n",
            ),
        ],
        std::slice::from_ref(&stdlib),
        Some(&jdk),
    );
    assert!(
        classes.is_none(),
        "an unresolved declared type must fail the compile, not emit classes"
    );
}

/// The same-package spelling stays resolvable without an import (control: the fix must not demand
/// imports for same-package references).
#[test]
fn same_package_type_without_import_still_resolves() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Foo.kt", "package app\nclass Foo\n"),
        ("Holder.kt", "package app\nclass Holder(val x: Foo)\n"),
    ]) else {
        return;
    };
    assert!(
        diags.is_empty(),
        "same-package resolution must not require an import, got: {diags:?}"
    );
}

/// An explicit import keeps the cross-package reference legal (control for the fix's scope).
#[test]
fn imported_cross_package_type_still_resolves() {
    let Some(diags) = common::module_front_end_diagnostics(&[
        ("Foo.kt", "package other\nclass Foo\n"),
        ("Holder.kt", "import other.Foo\nclass Holder(val x: Foo)\n"),
    ]) else {
        return;
    };
    assert!(
        diags.is_empty(),
        "an imported cross-package type must resolve, got: {diags:?}"
    );
}
