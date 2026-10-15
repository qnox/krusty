//! Whole source sets analyzed against the Kotlin/Native stdlib KLIB as their platform.

use super::super::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use crate::diag::DiagSink;

pub(super) fn target_stdlib(
    target: crate::compilation_target::CompilationTarget,
) -> Option<KlibLibraries> {
    let stdlib = crate::toolchain::kotlin_stdlib_klib(target)?;
    Some(
        KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            stdlib,
            match target {
                crate::compilation_target::CompilationTarget::Native => KlibStdlibSemantics::Native,
                crate::compilation_target::CompilationTarget::WasmJs
                | crate::compilation_target::CompilationTarget::WasmWasi => {
                    KlibStdlibSemantics::Wasm
                }
                crate::compilation_target::CompilationTarget::Js => KlibStdlibSemantics::Plain,
                crate::compilation_target::CompilationTarget::Jvm => return None,
            },
        )])
        .unwrap_or_else(|error| panic!("{error}")),
    )
}

pub(super) fn native_stdlib() -> Option<KlibLibraries> {
    let libraries = target_stdlib(crate::compilation_target::CompilationTarget::Native);
    assert!(
        libraries.is_some() || std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
        "KRUSTY_REQUIRE_KLIB is set but no Kotlin/Native stdlib KLIB is provisioned"
    );
    libraries
}

fn diagnostics(libraries: &KlibLibraries, source: &str) -> Vec<String> {
    let mut diags = DiagSink::new();
    // The provider names no target; this analysis is Native's because these are its libraries.
    let platform = crate::frontend::PlatformProvider::new(
        crate::compilation_target::CompilationTarget::Native,
        Box::new(libraries.for_compilation()),
    );
    let _ = crate::frontend::analyze_source(source, platform, &mut diags);
    diags
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.clone())
        .collect()
}

#[test]
fn stdlib_declarations_resolve_from_the_klib() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let source = r#"
fun box(): String {
    val clamped = 5.coerceIn(0, 3)
    if (clamped != 3) return "FAIL"
    val builder = StringBuilder()
    builder.append(listOf("O", "K").size)
    println(builder.toString())
    try {
        throw IllegalStateException("thrown")
    } catch (e: Exception) {
        return "OK"
    }
}
"#;
    assert_eq!(diagnostics(&libraries, source), Vec::<String>::new());
}

#[test]
fn function_types_are_the_language_classifiers() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let source = r#"
fun box(): String {
    val ok: Function0<String> = { "OK" }
    return ok()
}
"#;
    assert_eq!(diagnostics(&libraries, source), Vec::<String>::new());
}

#[test]
fn a_contract_block_selects_the_klib_dsl_members() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let source = r#"
import kotlin.contracts.*

@OptIn(ExperimentalContracts::class)
fun isString(value: Any?): Boolean {
    contract { returns(true) implies (value is String) }
    return value is String
}

@OptIn(ExperimentalContracts::class)
fun once(block: () -> Unit) {
    contract { callsInPlace(block, InvocationKind.EXACTLY_ONCE) }
    block()
}

fun box(): String {
    val value: Any? = "OK"
    val result: String
    once { result = "" }
    if (isString(value)) return value + result
    return "FAIL"
}
"#;
    assert_eq!(diagnostics(&libraries, source), Vec::<String>::new());
}

#[test]
fn a_vararg_annotation_applies_with_its_elements() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let source = r#"
@Suppress("UNUSED_VARIABLE", "UNUSED_PARAMETER")
fun box(): String = "OK"
"#;
    assert_eq!(diagnostics(&libraries, source), Vec::<String>::new());
}

#[test]
fn an_omitted_constant_default_is_the_klib_parameter_default() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let source = r#"
import kotlin.test.assertEquals

fun box(): String {
    assertEquals(3, 1 + 2)
    val joined = listOf("O", "K").joinToString("")
    return joined
}
"#;
    assert_eq!(diagnostics(&libraries, source), Vec::<String>::new());
}

#[test]
fn an_omitted_constant_constructor_default_is_the_klib_parameter_default() {
    let Some(libraries) = native_stdlib() else {
        return;
    };
    let source = r#"
fun box(): String {
    try {
        throw NotImplementedError()
    } catch (e: NotImplementedError) {
        return "OK"
    }
}
"#;
    assert_eq!(diagnostics(&libraries, source), Vec::<String>::new());
}
