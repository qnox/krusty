//! A classpath `typealias` declared in a file facade that `@JvmPackageName` RELOCATED — the alias's
//! declaring Kotlin package is NOT the JVM package its facade class file sits in.
//!
//! `kotlin-test`'s JUnit5 variant is the shape every Kotlin test source in the wild hits:
//!
//! ```text
//! @file:JvmPackageName("kotlin.test.junit5.annotations")
//! package kotlin.test
//! public actual typealias Test = org.junit.jupiter.api.Test
//! ```
//!
//! The facade class is `kotlin/test/junit5/annotations/AnnotationsKt`, but `Test` is declared in
//! package `kotlin.test`. krusty keyed every classpath alias by the JVM parent directory of its
//! facade, so the alias landed under `kotlin/test/junit5/annotations/Test` and `import
//! kotlin.test.Test` reported `unresolved reference 'Test'` for every `@Test`-annotated function.
//!
//! The dependency is compiled by the REFERENCE compiler on purpose: the contract under test is
//! krusty CONSUMING kotlinc's relocated-facade `@Metadata` (`pn`) and the matching `kotlin_module`
//! catalog entry. `@JvmPackageName` is `internal` to the stdlib, so the fixture is built with
//! `-Xfriend-paths` pointed at kotlin-stdlib, exactly as the kotlin-test build does.
use super::common;
use std::path::PathBuf;

const MARKER: &str = "package aliases\n\
    annotation class Marker\n\
    class Payload(val n: Int) { fun get(): Int = n }\n";

/// The relocated facade: declared package `aliases`, emitted to `aliases/relocated/RelocatedKt`.
/// The top-level function and property are not decoration — unlike the aliases, they resolve through
/// the package's FACADE CATALOG, which is the channel a classpath entry without a `kotlin_module`
/// can only populate from each facade's own `@Metadata`.
const RELOCATED: &str = "@file:JvmPackageName(\"aliases.relocated\")\n\
    package aliases\n\
    typealias Tag = Marker\n\
    typealias Cargo = Payload\n\
    val standard: Cargo = Payload(4)\n\
    fun cargoOf(n: Int): Cargo = Payload(n)\n";

/// Compiled ONCE per test process and shared: the tests run in parallel, so each building its own
/// copy would multiply a kotlinc invocation across every case for no added coverage.
fn kotlinc_lib() -> Option<PathBuf> {
    static LIB_OUT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    LIB_OUT
        .get_or_init(|| {
            // `scratch_dir` hands out a unique per-call directory under the harness's own root, so
            // the stale-scratch sweeper reclaims it if this process dies mid-run.
            let dir = common::scratch_dir()?;
            let marker = dir.join("Marker.kt");
            std::fs::write(&marker, MARKER).ok()?;
            let relocated = dir.join("Relocated.kt");
            std::fs::write(&relocated, RELOCATED).ok()?;
            let out = dir.join("classes");
            std::fs::create_dir_all(&out).ok()?;
            let (code, stderr) = common::kotlinc_compile(&[
                marker.to_string_lossy().into_owned(),
                relocated.to_string_lossy().into_owned(),
                "-d".to_string(),
                out.to_string_lossy().into_owned(),
                format!("-Xfriend-paths={}", common::stdlib_jar().display()),
            ])?;
            assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
            // The relocation is the whole point of the fixture: a build that quietly stopped
            // honouring `@JvmPackageName` would leave these tests passing for the wrong reason.
            assert!(
                out.join("aliases/relocated/RelocatedKt.class").is_file(),
                "fixture facade was not relocated by @JvmPackageName"
            );
            Some(out)
        })
        .clone()
}

/// The same fixture with `META-INF/` removed — a separate-compilation output directory, which is
/// what a build tool puts on the classpath and which carries NO `kotlin_module`. Without that
/// catalog the declaring package can only come from the facade's own `@Metadata` `pn`, so this is
/// the case that pins the class-directory facade recovery rather than the `kotlin_module` reader.
fn kotlinc_lib_without_module() -> Option<PathBuf> {
    static LIB_OUT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    LIB_OUT
        .get_or_init(|| {
            let source = kotlinc_lib()?;
            let dir = common::scratch_dir()?.join("no-module");
            copy_tree(&source, &dir).ok()?;
            std::fs::remove_dir_all(dir.join("META-INF")).ok()?;
            Some(dir)
        })
        .clone()
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The alias in a CODE position only. A declared type spelled through an alias also needs
/// `Type.abbreviatedType` in the consumer's own `@Metadata`, which krusty does not write for ANY
/// classpath alias yet (relocated or not) — a separate writer gap, tracked on its own. Everything
/// this fix governs — which class the alias names, and therefore the emitted bytecode — is proven
/// here.
const CONSUMER: &str = "package use\n\
    import aliases.Cargo\n\
    class Sample {\n\
    \x20   fun make(): Int = Cargo(7).get()\n\
    }\n";

/// The alias resolves and behaves as its target: as a constructor, in a parameter type, and as an
/// annotation on a member.
#[test]
fn relocated_facade_typealias_resolves_and_runs() {
    let Some(libout) = kotlinc_lib() else { return };
    run_alias_box(libout);
}

/// The same contract for a classpath entry with no `kotlin_module` — the declaring package then has
/// exactly one carrier, the facade's own `@Metadata`.
#[test]
fn relocated_facade_typealias_resolves_without_a_kotlin_module() {
    let Some(libout) = kotlinc_lib_without_module() else {
        return;
    };
    assert!(
        !libout.join("META-INF").exists(),
        "fixture still carries a kotlin_module"
    );
    run_alias_box(libout);
}

fn run_alias_box(libout: PathBuf) {
    let main = "import aliases.Cargo\n\
        import aliases.Tag\n\
        import aliases.cargoOf\n\
        import aliases.standard\n\
        class Sample {\n\
        \x20 @Tag\n\
        \x20 fun tagged(): Int = Cargo(7).get()\n\
        }\n\
        fun useParam(c: Cargo): Int = c.get()\n\
        fun box(): String {\n\
        \x20 val c = Cargo(5)\n\
        \x20 if (c.get() != 5) return \"fail ctor: ${c.get()}\"\n\
        \x20 if (useParam(Cargo(3)) != 3) return \"fail param\"\n\
        \x20 if (Sample().tagged() != 7) return \"fail annotated member: ${Sample().tagged()}\"\n\
        \x20 if (cargoOf(6).get() != 6) return \"fail top-level fun: ${cargoOf(6).get()}\"\n\
        \x20 if (standard.get() != 4) return \"fail top-level property: ${standard.get()}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let jdk = common::jdk_modules();
    let result = common::expect_box_run(
        main,
        "Main",
        &[libout, common::stdlib_jar()],
        Some(jdk.as_path()),
    );
    assert_eq!(result, "OK");
}

/// Byte identity against kotlinc for the consumer class. Resolving the alias is not enough: it must
/// expand to the alias's TARGET (`aliases/Payload`) in the constant pool, the `new`/`invokespecial`
/// operands and the method descriptors, exactly as the reference compiler writes them.
#[test]
fn relocated_facade_typealias_consumer_is_byte_identical() {
    let Some(libout) = kotlinc_lib() else { return };
    let Some(dir) = common::scratch_dir() else {
        return;
    };
    let source = dir.join("Use.kt");
    std::fs::write(&source, CONSUMER).expect("write consumer source");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("create reference output directory");
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        libout.to_string_lossy().into_owned(),
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
    ])
    .expect("kotlinc is available");
    assert_eq!(code, 0, "kotlinc rejected the consumer: {stderr}");
    let expected =
        std::fs::read(reference.join("use/Sample.class")).expect("kotlinc emitted use/Sample");

    let jdk = common::jdk_modules();
    let classes = common::expect_compile_in_process(
        CONSUMER,
        "Use",
        &[libout, common::stdlib_jar()],
        Some(jdk.as_path()),
    );
    let (_, actual) = classes
        .iter()
        .find(|(name, _)| name == "use/Sample")
        .expect("krusty emitted use/Sample");

    // The reference class stays on disk until the comparison has passed: it is the only thing that
    // makes a byte difference diagnosable.
    assert_eq!(
        actual,
        &expected,
        "use/Sample.class differs from kotlinc (krusty {} B, kotlinc {} B, first differing byte at {:?})",
        actual.len(),
        expected.len(),
        actual
            .iter()
            .zip(expected.iter())
            .position(|(a, b)| a != b)
    );
}
