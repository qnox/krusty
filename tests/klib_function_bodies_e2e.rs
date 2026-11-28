//! Top-level function bodies as krusty's KLIB writer records them, read and linked by kotlinc.
//!
//! Each library is a shared regression of `tests/box_regressions/multimodule/`. krusty compiles its
//! library module into a Native KLIB; the pinned Kotlin/Native distribution's `klib dump-ir`
//! deserializes that KLIB's IR, and `kotlinc-native` links the shared program's main module
//! against it and runs it. Both directions use the official consumer, so a nonstandard table, a
//! symbol of the wrong kind or a call by an unknown signature fails here rather than only in
//! krusty's own reader.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::diag::DiagSink;
use krusty::klib::backend::KlibBackend;
use krusty::klib::write::{KlibPlatform, KlibStamp};

use crate::common;

const PROGRAM: &str = include_str!("box_regressions/multimodule/libraryTopLevelFunctionBodies.kt");
const CLASS_PROGRAM: &str = include_str!("box_regressions/multimodule/libraryClassBodies.kt");
const CLASS_FORMS_PROGRAM: &str = include_str!("box_regressions/multimodule/libraryClassForms.kt");
const LOOPS: &str = include_str!("box_regressions/multimodule/libraryLoopBodies.kt");
const TRIES: &str = include_str!("box_regressions/multimodule/libraryTryBodies.kt");
const LAMBDAS: &str = include_str!("box_regressions/multimodule/libraryLambdaBodies.kt");
const VARARGS: &str = include_str!("box_regressions/multimodule/libraryVarargBodies.kt");
const INLINE_FUNCTIONS: &str =
    include_str!("box_regressions/multimodule/libraryInlineFunctionBodies.kt");
const INLINE_MEMBERS: &str =
    include_str!("box_regressions/multimodule/publicInlineMemberNonLocalReturnAcrossModules.kt");
const REIFIED_INLINE_FUNCTIONS: &str =
    include_str!("box_regressions/multimodule/multimoduleReifiedCallableOrigins.kt");

/// The provisioned reference Kotlin/Native distribution, whose stdlib the library compiles against.
/// A missing distribution fails the oracle rather than skipping it.
fn kotlin_native_root() -> PathBuf {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib_or_reason(
        krusty::compilation_target::CompilationTarget::Native,
    )
    .unwrap_or_else(|reason| panic!("the KLIB body oracle needs Kotlin/Native: {reason}"));
    // The stdlib is `<root>/klib/common/stdlib`.
    let root = stdlib
        .ancestors()
        .nth(3)
        .expect("the Native stdlib sits inside its distribution")
        .to_path_buf();
    assert!(
        root.join("bin/kotlinc-native").is_file(),
        "the Kotlin/Native distribution at {} has no bin/kotlinc-native",
        root.display()
    );
    root
}

/// The shared program's library and main sources.
fn program_sources(program: &str) -> (String, String) {
    let modules = krusty::conformance::split_modules(program).expect("a two-module program");
    let [library, main] = modules.as_slice() else {
        panic!("the shared program has a library and a main module");
    };
    assert_eq!(main.deps, std::slice::from_ref(&library.name));
    let [(_, library_source)] = library.files.as_slice() else {
        panic!("the library module is one Kotlin file");
    };
    let [(_, main_source)] = main.files.as_slice() else {
        panic!("the main module is one Kotlin file");
    };
    (library_source.clone(), main_source.clone())
}

/// Compile `source` with krusty into an unpacked linux_x64 KLIB under `work`.
fn write_library(work: &Path, source: &str) -> PathBuf {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    )
    .expect("the Native KLIB producer test has a stdlib");
    let libraries = krusty::klib_libraries::KlibLibraries::open(&[
        krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
            stdlib,
            krusty::klib_libraries::KlibStdlibSemantics::Native,
        ),
    ])
    .expect("open the Native stdlib KLIB");
    let inputs = [krusty::source::SourceInput::kotlin(source).with_file_stem("Library")];
    let features = krusty::features::LangFeatures::from_source_in_environment(source);
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &features,
        &mut diags,
    );
    let backend = KlibBackend::new(
        KlibPlatform::Native,
        KlibStamp::new(
            krusty::kotlin_version::environment_release(),
            features.language_version(),
        ),
    )
    .with_native_target(krusty_native::NativeTarget::new(
        krusty_native::Arch::X86_64,
        krusty_native::Os::Linux,
    ))
    .expect("the official consumer fixture targets Kotlin/Native linux_x64");
    let entries = krusty::compiler::emit_analyzed(
        analysis,
        &["Library".to_owned()],
        &backend,
        "lib",
        &mut diags,
    );
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    assert!(
        entries
            .iter()
            .any(|(path, _)| path == "default/ir/bodies.knb"),
        "every function of the library has a body the writer records"
    );
    let library = work.join("lib");
    for (entry, bytes) in entries {
        let path = library.join(entry);
        std::fs::create_dir_all(path.parent().expect("KLIB entry has a parent"))
            .expect("create KLIB entry directory");
        std::fs::write(path, bytes).expect("write KLIB entry");
    }
    library
}

fn scratch(name: &str) -> PathBuf {
    let work = common::scratch_dir()
        .expect("allocate the official KLIB consumer directory")
        .join(name);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("create the consumer directory");
    work
}

/// kotlinc-native links the main module of `program` against the KLIB krusty writes for its
/// library module, and the program prints `OK`.
fn kotlinc_native_runs(program: &str, name: &str) {
    let native_root = kotlin_native_root();
    let (library_source, main_source) = program_sources(program);
    let work = scratch(&format!("krusty-{name}-official-consumer"));
    let library = write_library(&work, &library_source);
    let main = work.join("Main.kt");
    std::fs::write(
        &main,
        format!("{main_source}\nfun main() {{ print(box()) }}\n"),
    )
    .expect("write the consumer source");
    let output = Command::new(native_root.join("bin/kotlinc-native"))
        .arg(&main)
        .arg("-library")
        .arg(&library)
        .args(["-produce", "program", "-output"])
        .arg(work.join("consumer"))
        .output()
        .expect("launch the pinned kotlinc-native consumer");
    assert!(
        output.status.success(),
        "kotlinc-native rejected krusty's function bodies (status {}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(work.join("consumer.kexe"))
        .output()
        .expect("run the official consumer");
    assert_eq!(
        (
            run.status.success(),
            String::from_utf8_lossy(&run.stdout).into_owned()
        ),
        (true, "OK".to_owned()),
        "stderr:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::remove_dir_all(work).expect("remove the consumer directory");
}

#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_function_bodies() {
    kotlinc_native_runs(PROGRAM, "function-bodies");
}

#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_loops() {
    kotlinc_native_runs(LOOPS, "loop-bodies");
}

#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_try_and_throw() {
    kotlinc_native_runs(TRIES, "try-bodies");
}

#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_lambdas() {
    kotlinc_native_runs(LAMBDAS, "lambda-bodies");
}

#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_varargs() {
    kotlinc_native_runs(VARARGS, "vararg-bodies");
}

/// kotlinc's own dump of a library's IR, with the source path it records left out. A signature is
/// printed by its mangled description when the library's debug table carries one and by its hash
/// otherwise; the dump keeps only the declaration name before it, as the kotlinc consumer test
/// proves each signature links.
fn dump_ir(native_root: &Path, library: &Path) -> String {
    dump(native_root, library, "dump-ir")
}

/// [`dump_ir`] of the prepared inline copies a library carries, by `klib dump-ir-inlinable-functions`.
fn dump_inlinable(native_root: &Path, library: &Path) -> String {
    dump(native_root, library, "dump-ir-inlinable-functions")
}

fn dump(native_root: &Path, library: &Path, command: &str) -> String {
    let output = Command::new(native_root.join("bin/klib"))
        .arg(command)
        .arg(library)
        .output()
        .expect("launch the pinned klib tool");
    assert!(
        output.status.success(),
        "klib {command} rejected the library (status {}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("klib dump-ir prints UTF-8")
        .lines()
        .filter(|line| !line.starts_with("Picked up "))
        .map(|line| {
            if let Some(file) = line.trim_start().strip_prefix("FILE fqName:") {
                return format!(
                    "  FILE fqName:{}",
                    file.split(' ')
                        .next()
                        .expect("a FILE line names its package")
                );
            }
            // `dump-ir-inlinable-functions` heads its dump with the library's path.
            match line.split_once(" inlinable functions in ") {
                Some((count, _)) if line.starts_with("// ") => count.to_owned(),
                _ => without_signature_descriptions(line),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `line` with each `|<description or hash>[` of a signature written `|[`.
fn without_signature_descriptions(line: &str) -> String {
    let mut rendered = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(bar) = rest.find('|') {
        rendered.push_str(&rest[..=bar]);
        rest = &rest[bar + 1..];
        // A description nests braces and brackets; the signature's own `[mask]` follows `}` or a
        // hash's last digit.
        let mut depth = 0usize;
        let end = rest.char_indices().find_map(|(index, char)| match char {
            '(' | '{' => {
                depth += 1;
                None
            }
            ')' | '}' => {
                depth = depth.saturating_sub(1);
                None
            }
            '[' if depth == 0 => Some(index),
            _ => None,
        });
        match end {
            Some(end) => rest = &rest[end..],
            None => break,
        }
    }
    rendered.push_str(rest);
    rendered
}

/// The dump of every function `names` lists, in dump order.
fn functions(dump: &str, names: &[&str]) -> String {
    let mut selected = String::new();
    let mut keep = false;
    for line in dump.lines() {
        if let Some(rest) = line.strip_prefix("    FUN name:") {
            keep = names
                .iter()
                .any(|name| rest.split(' ').next() == Some(*name));
        } else if !line.starts_with("     ") {
            keep = false;
        }
        if keep {
            selected.push_str(line);
            selected.push('\n');
        }
    }
    selected
}

/// The functions whose bodies common IR records in the very form kotlinc's serializer writes:
/// their whole dump is kotlinc's. (An `if` is a `when` whose `IF` origin common IR does not keep.)
const KOTLINC_SHAPED: [&str; 6] = ["answer", "sum", "greet", "isNegative", "differs", "twice"];

/// kotlinc's `klib dump-ir` prints each function `shaped` names identically for the KLIB krusty
/// writes from `program`'s library module and the one kotlinc-native writes from it.
fn kotlinc_reads_as_it_writes(program: &str, name: &str, shaped: &[&str]) {
    let native_root = kotlin_native_root();
    let (library_source, _) = program_sources(program);
    let work = scratch(&format!("krusty-{name}-dump"));
    let krusty_library = write_library(&work, &library_source);
    let kotlinc_library = kotlinc_library(&native_root, &work, &library_source);
    let krusty = dump_ir(&native_root, &krusty_library);
    let kotlinc = dump_ir(&native_root, &kotlinc_library);
    assert_eq!(
        functions(&krusty, shaped),
        functions(&kotlinc, shaped),
        "the whole krusty dump:\n{krusty}"
    );
    std::fs::remove_dir_all(work).expect("remove the dump directory");
}

/// The library's whole dump, with the one fact common IR does not record: whether a `this` a
/// member body reads was written or implied, which kotlinc's `IMPLICIT_ARGUMENT` origin says.
#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_class_bodies() {
    kotlinc_native_runs(CLASS_PROGRAM, "class-bodies");
}

#[test]
fn kotlinc_native_runs_a_program_against_krusty_written_class_forms() {
    kotlinc_native_runs(CLASS_FORMS_PROGRAM, "class-forms");
}

#[test]
fn kotlinc_reads_krusty_written_class_bodies_as_it_writes_them() {
    assert_kotlinc_dump(CLASS_PROGRAM, "krusty-class-bodies-dump");
}

#[test]
fn kotlinc_reads_krusty_written_class_forms_as_it_writes_them() {
    assert_kotlinc_dump(CLASS_FORMS_PROGRAM, "krusty-class-forms-dump");
}

/// `klib dump-ir` prints the same tree for the library module of `program` whether krusty or
/// kotlinc-native wrote it. Common IR does not record which receivers the source left implicit,
/// so kotlinc's `IMPLICIT_ARGUMENT` origin reads as no origin.
fn assert_kotlinc_dump(program: &str, name: &str) {
    let native_root = kotlin_native_root();
    let (library_source, _) = program_sources(program);
    let work = scratch(name);
    let krusty_library = write_library(&work, &library_source);
    let kotlinc_library = kotlinc_library(&native_root, &work, &library_source);
    let krusty = dump_ir(&native_root, &krusty_library);
    let kotlinc =
        dump_ir(&native_root, &kotlinc_library).replace("origin=IMPLICIT_ARGUMENT", "origin=null");
    assert_eq!(krusty, kotlinc);
    std::fs::remove_dir_all(work).expect("remove the dump directory");
}

/// kotlinc-native's own unpacked KLIB of `source`, under `work`.
fn kotlinc_library(native_root: &Path, work: &Path, source: &str) -> PathBuf {
    let kotlinc_source = work.join("Library.kt");
    std::fs::write(&kotlinc_source, source).expect("write the library source");
    let library = work.join("kotlinc-lib");
    let output = Command::new(native_root.join("bin/kotlinc-native"))
        .arg(&kotlinc_source)
        .args([
            "-produce",
            "library",
            "-nopack",
            "-module-name",
            "lib",
            "-output",
        ])
        .arg(&library)
        .output()
        .expect("launch the pinned kotlinc-native");
    assert!(
        output.status.success(),
        "kotlinc-native failed (status {}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    library
}

#[test]
fn kotlinc_reads_krusty_written_function_bodies_as_it_writes_them() {
    kotlinc_reads_as_it_writes(PROGRAM, "function-bodies", &KOTLINC_SHAPED);
}

#[test]
fn kotlinc_reads_krusty_written_varargs_as_it_writes_them() {
    kotlinc_reads_as_it_writes(
        VARARGS,
        "vararg-bodies",
        &["count", "listed", "spread", "spelled", "built"],
    );
}

#[test]
fn kotlinc_native_inlines_krusty_written_inline_functions() {
    kotlinc_native_runs(INLINE_FUNCTIONS, "inline-functions");
}

/// The prepared inline copies krusty writes are the ones kotlinc-native writes, as its own
/// `klib dump-ir-inlinable-functions` prints them, and the library's declarations match too.
#[test]
fn kotlinc_reads_krusty_written_inline_functions_as_it_writes_them() {
    let native_root = kotlin_native_root();
    let (library_source, _) = program_sources(INLINE_FUNCTIONS);
    let work = scratch("krusty-inline-functions-dump");
    let krusty_library = write_library(&work, &library_source);
    let kotlinc_library = kotlinc_library(&native_root, &work, &library_source);
    let implicit = |dump: String| dump.replace("origin=IMPLICIT_ARGUMENT", "origin=null");
    assert_eq!(
        dump_inlinable(&native_root, &krusty_library),
        implicit(dump_inlinable(&native_root, &kotlinc_library)),
    );
    assert_eq!(
        dump_ir(&native_root, &krusty_library),
        implicit(dump_ir(&native_root, &kotlinc_library)),
    );
    std::fs::remove_dir_all(work).expect("remove the dump directory");
}

/// A prepared copy declares every parameter a regular one: a dispatch receiver, context
/// parameters and an extension receiver, in that order, as kotlinc-native writes them.
#[test]
fn kotlinc_reads_krusty_written_inline_copies_of_every_parameter_kind() {
    let native_root = kotlin_native_root();
    let (library_source, _) = program_sources(REIFIED_INLINE_FUNCTIONS);
    let work = scratch("krusty-inline-parameter-kinds-dump");
    let krusty_library = write_library(&work, &library_source);
    let kotlinc_library = kotlinc_library(&native_root, &work, &library_source);
    assert_eq!(
        dump_inlinable(&native_root, &krusty_library),
        dump_inlinable(&native_root, &kotlinc_library),
    );
    std::fs::remove_dir_all(work).expect("remove the dump directory");
}

#[test]
fn kotlinc_native_inlines_krusty_written_public_inline_members() {
    kotlinc_native_runs(INLINE_MEMBERS, "inline-members");
}

/// A class's public inline members are written with the prepared copies kotlinc-native writes,
/// including a receiver lambda that returns non-locally with a value and with Unit.
#[test]
fn kotlinc_reads_krusty_written_inline_members_as_it_writes_them() {
    let native_root = kotlin_native_root();
    let (library_source, _) = program_sources(INLINE_MEMBERS);
    let work = scratch("krusty-inline-members-dump");
    let krusty_library = write_library(&work, &library_source);
    let kotlinc_library = kotlinc_library(&native_root, &work, &library_source);
    let implicit = |dump: String| dump.replace("origin=IMPLICIT_ARGUMENT", "origin=null");
    assert_eq!(
        dump_inlinable(&native_root, &krusty_library),
        implicit(dump_inlinable(&native_root, &kotlinc_library)),
    );
    assert_eq!(
        dump_ir(&native_root, &krusty_library),
        implicit(dump_ir(&native_root, &kotlinc_library)),
    );
    std::fs::remove_dir_all(work).expect("remove the dump directory");
}
