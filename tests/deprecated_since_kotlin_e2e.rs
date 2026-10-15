//! `@DeprecatedSinceKotlin(hiddenSince = ...)` is an API-version availability boundary on a
//! dependency declaration. The JVM provider decodes the declaration identity once and withholds
//! functions, properties and constructors before candidate construction.

use std::path::PathBuf;

use super::common;

const MAX: &str = "fun largest(values: List<Long>): Long = values.max()\n\
    fun box(): String {\n\
    \x20 val largest = largest(listOf(3L, 7L, 5L))\n\
    \x20 return if (largest == 7L) \"OK\" else \"$largest\"\n\
    }\n";

#[test]
fn the_current_max_overload_is_selected_as_kotlinc_selects_it() {
    let classes = common::classes_against_kotlinc_module(&[("Largest.kt", MAX)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn the_current_max_overload_runs() {
    common::expect_box_ok_with_stdlib(MAX, "max() past its hidden predecessor");
}

const GATED: &str = "package kotlin.gates\n\
    @Deprecated(\"Use fresh instead.\")\n\
    @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
    fun stale(): String = \"stale\"\n";

const CALLER: &str = "import kotlin.gates.stale\nfun read(): String = stale()\n";

/// A library compiled by kotlinc from `library`. Only the `kotlin` package may use
/// `@DeprecatedSinceKotlin`, so the fixture explicitly lifts that package reservation.
fn gated_library(library: &str) -> PathBuf {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let source = work.join("Gates.kt");
    std::fs::write(&source, library).expect("write the library");
    let output = work.join("lib");
    let arguments = [
        source.to_string_lossy().into_owned(),
        "-Xallow-kotlin-package".to_string(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&arguments).expect("the reference kotlinc");
    assert_eq!((code, stderr.as_str()), (0, ""), "kotlinc library");
    output
}

fn compile_at(library: &str, caller: &str, api: &str) -> common::CompilerDiagnosticResult {
    let classpath = [gated_library(library), common::stdlib_jar()];
    let arguments = ["-language-version", "2.4", "-api-version", api].map(str::to_string);
    common::compiler_diagnostics_with_shared_args(&[("Caller.kt", caller)], &classpath, &arguments)
}

fn compile_caller_at(api: &str) -> common::CompilerDiagnosticResult {
    compile_at(GATED, CALLER, api)
}

#[test]
fn a_callable_is_visible_below_its_hidden_since_level() {
    let result = compile_caller_at("2.2");
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc at API 2.2"
    );
    assert_eq!(
        (
            result.krusty_code,
            common::compiler_errors(&result.krusty_stderr)
        ),
        (0, Vec::new()),
        "krusty at API 2.2"
    );
}

#[test]
fn a_callable_is_unresolved_from_its_hidden_since_level() {
    let result = compile_caller_at("2.3");
    let expected = common::compiler_errors(&result.reference_stderr);
    assert_eq!(expected.len(), 1, "kotlinc: {}", result.reference_stderr);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
}

/// A hidden dependency name remains importable; only its use is unresolved.
#[test]
fn an_import_of_a_hidden_declaration_resolves() {
    let library = "package kotlin.gates\n\
        @Deprecated(\"Use fresh instead.\", level = DeprecationLevel.HIDDEN)\n\
        fun stale(): String = \"stale\"\n";
    let classpath = [gated_library(library), common::stdlib_jar()];
    let result = common::compiler_diagnostics(&[("Caller.kt", CALLER)], &classpath);
    let expected = common::compiler_errors(&result.reference_stderr);
    assert_eq!(expected.len(), 1, "kotlinc: {}", result.reference_stderr);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
}

fn assert_hidden_since_boundary(library: &str, caller: &str, declaration: &str) {
    let visible = compile_at(library, caller, "2.2");
    assert_eq!(
        (visible.reference_code, visible.reference_stderr.as_str()),
        (0, ""),
        "kotlinc sees {declaration} below hiddenSince"
    );
    assert_eq!(
        (
            visible.krusty_code,
            common::compiler_errors(&visible.krusty_stderr)
        ),
        (0, Vec::new()),
        "krusty sees {declaration} below hiddenSince"
    );

    let hidden = compile_at(library, caller, "2.3");
    let expected = common::compiler_errors(&hidden.reference_stderr);
    assert_eq!(
        expected.len(),
        1,
        "kotlinc hides {declaration} at hiddenSince: {}",
        hidden.reference_stderr
    );
    assert_eq!(common::compiler_errors(&hidden.krusty_stderr), expected);
}

#[test]
fn a_property_is_hidden_from_its_hidden_since_level() {
    let library = "package kotlin.gates\n\
        @Deprecated(\"Use freshValue instead.\")\n\
        @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
        val staleValue: String get() = \"stale\"\n";
    let caller = "import kotlin.gates.staleValue\nfun read(): String = staleValue\n";
    assert_hidden_since_boundary(library, caller, "property");
}

#[test]
fn a_const_property_is_hidden_from_its_hidden_since_level() {
    let library = "package kotlin.gates\n\
        @Deprecated(\"Use freshValue instead.\")\n\
        @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
        const val staleValue: String = \"stale\"\n";
    let caller = "import kotlin.gates.staleValue\nfun read(): String = staleValue\n";
    assert_hidden_since_boundary(library, caller, "const property");
}

const INSTANCE_JVM_FIELD: &str = "package kotlin.gates\n\
    class Gates {\n\
    \x20   @JvmField\n\
    \x20   @Deprecated(\"Use freshValue instead.\")\n\
    \x20   @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
    \x20   val staleInstance: String = \"stale\"\n\
    }\n";

#[test]
fn an_instance_jvm_field_is_hidden_from_its_hidden_since_level() {
    let caller = "import kotlin.gates.Gates\n\
        fun read(gates: Gates): String = gates.staleInstance\n";
    assert_hidden_since_boundary(INSTANCE_JVM_FIELD, caller, "instance @JvmField property");
}

#[test]
fn the_provider_withholds_an_instance_jvm_field_at_its_hidden_since_level() {
    use krusty::jvm::classpath::Classpath;
    use krusty::jvm::jvm_libraries::JvmLibraries;
    use krusty::language_version::LanguageVersion;
    use krusty::symbol_source::SymbolSource;
    use krusty::types::{type_name, Ty};

    let classpath = std::rc::Rc::new(Classpath::new(vec![
        gated_library(INSTANCE_JVM_FIELD),
        common::stdlib_jar(),
    ]));
    let provider = JvmLibraries::new(classpath)
        .expect("JVM provider")
        .with_api_version(LanguageVersion::V2_3);
    let classifier = provider
        .classifier(type_name("kotlin/gates/Gates"))
        .expect("Gates classifier");
    let [property] = classifier
        .declared_callables
        .get("staleInstance")
        .expect("field declaration")
        .properties()
    else {
        panic!("one field declaration");
    };
    assert!(property.deprecated_hidden);
    assert!(
        krusty::symbol_resolver::SymbolResolver::new(&provider)
            .select_member_property(Ty::obj("kotlin/gates/Gates"), "staleInstance")
            .is_none(),
        "ordinary member selection must withhold the API-normalized field"
    );
}

#[test]
fn the_provider_withholds_a_const_property_at_its_hidden_since_level() {
    use krusty::jvm::classpath::Classpath;
    use krusty::jvm::jvm_libraries::JvmLibraries;
    use krusty::language_version::LanguageVersion;
    use krusty::libraries::{Callables, SemanticPlatform};
    use krusty::symbol_source::{SymbolNamespace, SymbolSource};
    use krusty::types::type_name;

    let library = "package kotlin.gates\n\
        @Deprecated(\"Use freshValue instead.\")\n\
        @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
        const val staleValue: String = \"stale\"\n";
    let classpath = std::rc::Rc::new(Classpath::new(vec![
        gated_library(library),
        common::stdlib_jar(),
    ]));
    let provider = JvmLibraries::new(classpath)
        .expect("JVM provider")
        .with_api_version(LanguageVersion::V2_3);
    let symbols = provider.symbols(
        SymbolNamespace::Package(type_name("kotlin/gates")),
        "staleValue",
    );
    assert!(matches!(symbols.callables, Callables::None));
    assert!(symbols.importable_declaration);
    assert!(
        provider
            .top_level_associated_property(type_name("kotlin/gates"), "staleValue")
            .is_none(),
        "the static-field fast path must honor the provider-normalized availability"
    );
}

#[test]
fn a_constructor_is_hidden_from_its_hidden_since_level() {
    let library = "package kotlin.gates\n\
        class Stale @Deprecated(\"Use Fresh instead.\")\n\
            @DeprecatedSinceKotlin(hiddenSince = \"2.3\") constructor()\n";
    let caller = "import kotlin.gates.Stale\nfun build(): Any = Stale()\n";
    assert_hidden_since_boundary(library, caller, "constructor");
}

#[test]
fn an_object_jvm_field_is_hidden_from_its_hidden_since_level() {
    let library = "package kotlin.gates\n\
        object Gates {\n\
        \x20   @JvmField\n\
        \x20   @Deprecated(\"Use freshValue instead.\")\n\
        \x20   @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
        \x20   val staleObject: String = \"stale\"\n\
        }\n";
    let caller = "import kotlin.gates.Gates\nfun read(): String = Gates.staleObject\n";
    assert_hidden_since_boundary(library, caller, "object @JvmField property");
}

#[test]
fn a_companion_jvm_field_is_hidden_from_its_hidden_since_level() {
    let library = "package kotlin.gates\n\
        class Holder {\n\
        \x20   companion object {\n\
        \x20       @JvmField\n\
        \x20       @Deprecated(\"Use freshValue instead.\")\n\
        \x20       @DeprecatedSinceKotlin(hiddenSince = \"2.3\")\n\
        \x20       val staleCompanion: String = \"stale\"\n\
        \x20   }\n\
        }\n";
    let caller = "import kotlin.gates.Holder\nfun read(): String = Holder.staleCompanion\n";
    assert_hidden_since_boundary(library, caller, "companion @JvmField property");
}
