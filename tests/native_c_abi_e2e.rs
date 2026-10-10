//! The public C ABI of a native module.
//!
//! A public top-level function whose parameters are primitives or `String` and whose result is one
//! of those types or `Unit` is declared in a C header and exported under that name. An `internal`
//! function is not declared and is not a dynamic export. A public function whose name or types
//! cannot be represented safely is named in the header and not declared: the module still runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use object::{Object, ObjectSymbol};

use krusty::backend::{Artifact, Backend as _};
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

use super::common;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-cabi-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    krusty::native::can_link(target).then_some(target)
}

fn compile(sources: &[(&str, &str)], module: &str) -> (Vec<Artifact>, Vec<String>) {
    let jar = krusty::toolchain::stdlib_jar().expect("the native tests require the stdlib jar");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let inputs = sources
        .iter()
        .map(|(stem, source)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect::<Vec<_>>();
    let mut features = krusty::features::LangFeatures::new();
    for (_, source) in sources {
        features.apply_source_directives(source);
    }
    let mut diags = DiagSink::new();
    let entry = if sources
        .iter()
        .any(|(_, source)| source.contains("fun box("))
    {
        krusty::native::Entry::Box
    } else {
        krusty::native::Entry::Main
    };
    let backend = CraneliftBackend::new(host().expect("checked by the caller"))
        .with_entry(entry)
        .verified();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(backend.compilation_target(), platform),
        &features,
        &mut diags,
    );
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, module, &mut diags);
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

fn header<'a>(artifacts: &'a [Artifact], module: &str) -> &'a str {
    let name = format!("{module}.h");
    let (_, bytes) = artifacts
        .iter()
        .find(|(artifact, _)| artifact == &name)
        .unwrap_or_else(|| panic!("no {name} among {:?}", artifact_names(artifacts)));
    std::str::from_utf8(bytes).expect("the header is UTF-8")
}

fn artifact_names(artifacts: &[Artifact]) -> Vec<&str> {
    artifacts.iter().map(|(name, _)| name.as_str()).collect()
}

fn objects(artifacts: &[Artifact]) -> Vec<&[u8]> {
    artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect()
}

/// Dynamic text symbols are the ABI a shared object would export. Hidden symbols stay in the link
/// and are not on that list.
fn dynamic_text_symbols(object: &[u8]) -> Vec<String> {
    let file = object::File::parse(object).expect("the object parses");
    let mut names = Vec::new();
    for symbol in file.symbols() {
        if symbol.is_undefined() || symbol.kind() != object::SymbolKind::Text {
            continue;
        }
        if symbol.scope() != object::SymbolScope::Dynamic {
            continue;
        }
        names.push(symbol.name().expect("a named symbol").to_string());
    }
    names.sort();
    names
}

fn hidden_text_symbols(object: &[u8]) -> Vec<String> {
    let file = object::File::parse(object).expect("the object parses");
    let mut names = Vec::new();
    for symbol in file.symbols() {
        if symbol.is_undefined() || symbol.kind() != object::SymbolKind::Text {
            continue;
        }
        if symbol.scope() != object::SymbolScope::Linkage {
            continue;
        }
        names.push(symbol.name().expect("a named symbol").to_string());
    }
    names.sort();
    names
}

const LIBRARY: &str = "\
package demo
val base: Int = 20
internal fun hidden(n: Int): Int = n
fun value(n: Int): Int = base + hidden(n)
fun label(): String = \"OK\"
";

const HEADER: &str = "\
/* Public C ABI. A function whose name or types cannot be represented safely is named here and not declared. */
#ifndef KRUSTY_DEFS_H
#define KRUSTY_DEFS_H
#include <stdbool.h>
#include <stdint.h>

typedef int8_t kt_byte;
typedef int16_t kt_short;
typedef int32_t kt_int;
typedef int64_t kt_long;
typedef uint16_t kt_char;
typedef float kt_float;
typedef double kt_double;
typedef bool kt_boolean;
typedef void *kt_ref;

kt_int demo_value__Int(kt_int n);
kt_ref demo_label(void);
#endif
";

#[test]
fn a_public_function_is_exported_and_an_internal_one_is_not() {
    let Some(_target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let (artifacts, diagnostics) = compile(&[("defs", LIBRARY)], "defs");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(header(&artifacts, "defs"), HEADER);
    let object = objects(&artifacts)
        .into_iter()
        .find(|object| {
            dynamic_text_symbols(object)
                .iter()
                .any(|name| name == "demo_value__Int")
        })
        .expect("the defining object exports demo_value__Int");
    assert_eq!(
        dynamic_text_symbols(object),
        vec!["demo_label".to_string(), "demo_value__Int".to_string()]
    );
    let hidden = hidden_text_symbols(object);
    assert!(
        hidden.iter().any(|name| name.starts_with("kt_mod_")),
        "the Kotlin symbol stays in the link, hidden: {hidden:?}"
    );
    assert!(
        hidden.iter().any(|name| name.starts_with("kt_fileinit_")),
        "the file initializer stays in the link, hidden: {hidden:?}"
    );
    assert!(
        dynamic_text_symbols(object)
            .iter()
            .all(|name| !name.starts_with("kt_mod_") && !name.starts_with("kt_fileinit_")),
        "module symbols are not dynamic exports"
    );
}

#[test]
fn a_c_caller_sees_the_initialized_value() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let compiler = c_compiler().expect("a C compiler for the caller");
    let (artifacts, diagnostics) = compile(&[("defs", LIBRARY)], "defs");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let scratch = Scratch::new("call");
    std::fs::write(scratch.path().join("defs.h"), header(&artifacts, "defs"))
        .expect("write header");
    let caller = "\
#include \"defs.h\"
void kt_runtime_init(void *stack_bottom);
void kt_exit(kt_int status);
void kt_program_entry(void) {
    char slot;
    kt_runtime_init(&slot);
    if (demo_value__Int(1) != 21) kt_exit(2);
    kt_exit(0);
}
";
    let caller_object = compile_c(&compiler, scratch.path(), caller);
    let mut inputs = objects(&artifacts);
    inputs.push(&caller_object);
    let image = krusty::native::link_program(&inputs, target).expect("link the C caller");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(Command::new(&executable).env_clear()).expect("run");
    assert!(
        output.status.success(),
        "exit {:?}\nstdout {:?}\nstderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_public_classifier_parameter_is_declined_by_name() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let source = "\
package demo
class Point(val x: Int)
fun take(p: Point): Int = p.x
fun box(): String = if (take(Point(7)) == 7) \"OK\" else \"FAIL\"
";
    let (artifacts, diagnostics) = compile(&[("use", source)], "use");
    assert!(
        diagnostics.is_empty(),
        "the Kotlin program still lowers: {diagnostics:?}"
    );
    assert_eq!(
        header(&artifacts, "use"),
        "\
/* Public C ABI. A function whose name or types cannot be represented safely is named here and not declared. */
#ifndef KRUSTY_USE_H
#define KRUSTY_USE_H
#include <stdbool.h>
#include <stdint.h>

typedef int8_t kt_byte;
typedef int16_t kt_short;
typedef int32_t kt_int;
typedef int64_t kt_long;
typedef uint16_t kt_char;
typedef float kt_float;
typedef double kt_double;
typedef bool kt_boolean;
typedef void *kt_ref;

/* krusty: the native backend does not support a public function `take` with parameter type demo.Point yet */
kt_ref demo_box(void);
#endif
"
    );
    let image = krusty::native::link_program(&objects(&artifacts), target).expect("link");
    let scratch = Scratch::new("point");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(Command::new(&executable).env_clear()).expect("run");
    assert!(output.status.success(), "{:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let answer = stdout
        .split_once(krusty::native::BOX_RESULT_FRAME)
        .map(|(_, answer)| answer)
        .filter(|answer| !answer.contains(krusty::native::BOX_RESULT_FRAME))
        .unwrap_or("");
    assert_eq!(answer, "OK");
}

#[test]
fn an_unrepresentable_source_name_is_refused_without_exporting_the_kotlin_symbol() {
    let Some(_target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let source = r#"
        package demo
        fun `dash-name`(n: Int): Int = n
        fun box(): String = if (`dash-name`(1) == 1) "OK" else "FAIL"
    "#;
    let (artifacts, diagnostics) = compile(&[("names", source)], "names");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        header(&artifacts, "names"),
        "\
/* Public C ABI. A function whose name or types cannot be represented safely is named here and not declared. */
#ifndef KRUSTY_NAMES_H
#define KRUSTY_NAMES_H
#include <stdbool.h>
#include <stdint.h>

typedef int8_t kt_byte;
typedef int16_t kt_short;
typedef int32_t kt_int;
typedef int64_t kt_long;
typedef uint16_t kt_char;
typedef float kt_float;
typedef double kt_double;
typedef bool kt_boolean;
typedef void *kt_ref;

/* krusty: public function `dash-name` has no safe C identifier */
kt_ref demo_box(void);
#endif
"
    );
    let object = objects(&artifacts)[0];
    // `box` selects the executable entry mode, whose runtime ABI has always exported
    // `kt_program_entry`. The rejected backticked declaration contributes no dynamic symbol.
    assert_eq!(
        dynamic_text_symbols(object),
        vec!["demo_box".to_string(), "kt_program_entry".to_string()]
    );
    assert!(
        hidden_text_symbols(object)
            .iter()
            .any(|name| name.starts_with("kt_mod_")),
        "the Kotlin implementation remains link-visible but hidden"
    );
}

#[test]
fn lossy_package_spellings_do_not_emit_duplicate_c_symbols() {
    let Some(target) = host() else {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    };
    let sources = [
        ("first", "package a_b\nfun c(): Int = 1"),
        ("second", "package a\nfun b_c(): Int = 2"),
        ("box", "fun box(): String = \"OK\""),
    ];
    let (artifacts, diagnostics) = compile(&sources, "collision");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        header(&artifacts, "collision"),
        "\
/* Public C ABI. A function whose name or types cannot be represented safely is named here and not declared. */
#ifndef KRUSTY_COLLISION_H
#define KRUSTY_COLLISION_H
#include <stdbool.h>
#include <stdint.h>

typedef int8_t kt_byte;
typedef int16_t kt_short;
typedef int32_t kt_int;
typedef int64_t kt_long;
typedef uint16_t kt_char;
typedef float kt_float;
typedef double kt_double;
typedef bool kt_boolean;
typedef void *kt_ref;

kt_int a_b_c(void);
/* krusty: public function `b_c` would duplicate C symbol `a_b_c` */
kt_ref box(void);
#endif
"
    );
    let exported = objects(&artifacts)
        .into_iter()
        .flat_map(dynamic_text_symbols)
        .collect::<Vec<_>>();
    assert_eq!(
        exported
            .iter()
            .filter(|name| name.as_str() == "a_b_c")
            .count(),
        1
    );
    let image = krusty::native::link_program(&objects(&artifacts), target).expect("link");
    assert!(!image.is_empty());
}

fn c_compiler() -> Option<String> {
    if let Ok(compiler) = std::env::var("KRUSTY_RUNTIME_CC") {
        if !compiler.trim().is_empty() {
            return Some(compiler);
        }
    }
    ["clang", "cc", "gcc"].into_iter().find_map(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| candidate.to_string())
    })
}

fn compile_c(compiler: &str, directory: &Path, source: &str) -> Vec<u8> {
    let program = directory.join("caller.c");
    std::fs::write(&program, source).expect("write the caller");
    let object = directory.join("caller.o");
    let output = Command::new(compiler)
        .args([
            "-std=c11",
            "-ffreestanding",
            "-nostdlib",
            "-fno-pic",
            "-fno-stack-protector",
            "-O0",
            "-c",
            "-o",
        ])
        .arg(&object)
        .arg(&program)
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "the caller must compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&object).expect("read the caller")
}
