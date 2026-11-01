//! Interop KLIB declarations selected through the Native KLIB provider and called with the C ABI.
//!
//! The first tests use a Kotlin/Native-built fixture, metadata-only, whose implementation is a
//! separate C object linked by hand. The last builds the interop KLIB with `krusty cinterop` from a
//! checked-in `.def` file and header, and links the program with the shared library that KLIB's
//! link facts name, as a program built against it links.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::backend::Backend as _;
use krusty::diag::DiagSink;
use krusty::native::{Arch, CraneliftBackend, Entry, NativeTarget};
use krusty::source::SourceInput;
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::type_name;

use super::common;

const SOURCE: &str = "\
@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)
import fixture.directc.krusty_fixture_sum

fun box(): String = if (krusty_fixture_sum(19, 23) == 42) \"OK\" else \"wrong\"
";

const POINTER_SOURCE: &str = "\
@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)
import fixture.directc.krusty_fixture_pointer

fun box(): String {
    krusty_fixture_pointer(null)
    return \"OK\"
}
";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-native-cinterop-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create cinterop scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_cinterop/direct_call.klib")
}

fn libraries(target: NativeTarget) -> Option<krusty::native::NativeKlibInputs> {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    )?;
    Some(
        krusty::native::NativeKlibInputs::open(
            &[
                krusty::klib_libraries::KlibLibraryInput::dependency(fixture()),
                krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
                    stdlib,
                    krusty::klib_libraries::KlibStdlibSemantics::Native,
                ),
            ],
            target,
        )
        .expect("open the cinterop fixture and Native stdlib KLIBs"),
    )
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
            .map(|_| candidate.to_owned())
    })
}

fn c_object(compiler: &str, scratch: &Path) -> Vec<u8> {
    let source = scratch.join("direct_call.c");
    std::fs::write(
        &source,
        "int krusty_fixture_sum(int left, int right) { return left + right; }\n",
    )
    .expect("write the C implementation");
    let object = scratch.join("direct_call.o");
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
        .arg(&source)
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "the C implementation must compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(object).expect("read the C implementation object")
}

#[test]
fn a_real_interop_klib_publishes_and_calls_its_exact_direct_c_target() {
    let Some(target) = NativeTarget::host()
        .filter(|target| target.arch == Arch::X86_64 && krusty::native::can_link(*target))
    else {
        return;
    };
    let Some(inputs) = libraries(target) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "required Native stdlib KLIB is unavailable"
        );
        return;
    };

    let libraries = inputs.libraries();
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("fixture/directc")),
        "krusty_fixture_sum",
    );
    let [function] = symbols.callables.functions() else {
        panic!(
            "expected one fixture function, found {}",
            symbols.callables.functions().len()
        );
    };
    let identity = function
        .callable
        .external_identity
        .expect("the fixture declaration has a provider identity");
    let realization = libraries
        .external_callable(identity)
        .expect("the provider realizes its assigned identity");
    let Some(krusty::libraries::KlibDeclarationSignature::Public(signature)) =
        &realization.declaration_signature
    else {
        panic!("the fixture function has a public KLIB identity")
    };
    assert_eq!(signature.mask(), 4, "the exact identity is Native interop");
    assert_eq!(
        realization
            .direct_c_function
            .as_ref()
            .expect("the provider authenticated the direct C target")
            .symbol(),
        "krusty_fixture_sum"
    );
    let (libraries, imports) = inputs.into_parts();
    assert!(
        imports.is_empty(),
        "the hand-linked direct-call fixture names no C libraries"
    );

    let inputs = [SourceInput::kotlin(SOURCE).with_file_stem("directCall")];
    let stems = ["directCall".to_owned()];
    let mut diags = DiagSink::new();
    let backend = CraneliftBackend::new(target)
        .with_entry(Entry::Box)
        .verified();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(backend.compilation_target(), Box::new(libraries)),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, "direct", &mut diags);
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );

    let scratch = Scratch::new();
    let c = c_object(&c_compiler().expect("a host C compiler"), &scratch.0);
    let mut objects = artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    objects.push(&c);
    let image = krusty::native::link_program(&objects, target).expect("link the Native program");
    let executable = scratch.0.join("program");
    std::fs::write(&executable, image).expect("write the Native program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("make the Native program executable");
    }
    let output = common::run_freshly_written(Command::new(&executable).env_clear())
        .expect("run the Native program");
    assert!(output.status.success(), "exit {:?}", output.status.code());
    let stdout = String::from_utf8(output.stdout).expect("the Native output is UTF-8");
    assert_eq!(
        stdout,
        format!("{}OK", krusty::native::BOX_RESULT_FRAME),
        "the direct C call answers through the real fixture"
    );
}

#[test]
fn an_authenticated_direct_c_pointer_declines_its_nullable_boxed_carrier_exactly() {
    let Some(target) = NativeTarget::host()
        .filter(|target| target.arch == Arch::X86_64 && krusty::native::can_link(*target))
    else {
        return;
    };
    let Some(inputs) = libraries(target) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "required Native stdlib KLIB is unavailable"
        );
        return;
    };

    let (libraries, imports) = inputs.into_parts();
    assert!(
        imports.is_empty(),
        "the direct-call fixture names no C libraries"
    );
    let inputs = [SourceInput::kotlin(POINTER_SOURCE).with_file_stem("directPointer")];
    let stems = ["directPointer".to_owned()];
    let mut diags = DiagSink::new();
    let backend = CraneliftBackend::new(target)
        .with_entry(Entry::Box)
        .verified();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(backend.compilation_target(), Box::new(libraries)),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, "pointer", &mut diags);
    assert!(artifacts.is_empty(), "a declined file must emit no object");
    assert_eq!(diags.diags.len(), 1, "one selected call declines once");
    assert_eq!(
        diags.render_all(&[("directPointer.kt", POINTER_SOURCE)]),
        "directPointer.kt:5:5: error: krusty: the native backend does not support a call of the C function `krusty_fixture_pointer` passing `kotlinx.cinterop.CValuesRef?`, which has no C representation here yet\n"
    );
}

const SHARED_SOURCE: &str = "\
@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)
import fixture.sharedc.krusty_shared_scale
import fixture.sharedc.krusty_shared_sum

fun box(): String {
    if (krusty_shared_sum(19, 23) != 42) return \"wrong sum\"
    if (krusty_shared_scale(-3_000_000_000L, 3) != -9_000_000_000L) return \"wrong scale\"
    return \"OK\"
}
";

const SHARED_IMPLEMENTATION: &str = "\
#include \"shared_call.h\"

int krusty_shared_sum(int left, int right) { return left + right; }
long long krusty_shared_scale(long long value, int factor) { return value * factor; }
";

/// The soname the `.def` file's `-lkrusty_shared_call` finds the implementation by.
const SHARED_LIBRARY: &str = "libkrusty_shared_call.so";

fn shared_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_cinterop")
}

/// Build the C implementation of `shared_call.h` as the shared library [`SHARED_LIBRARY`] in
/// `directory`, named by that soname.
fn shared_library(compiler: &str, directory: &Path) {
    let source = directory.join("shared_call.c");
    std::fs::write(&source, SHARED_IMPLEMENTATION).expect("write the C implementation");
    let output = Command::new(compiler)
        .args(["-std=c11", "-O0", "-shared", "-fPIC", "-I"])
        .arg(shared_fixtures())
        .arg(format!("-Wl,-soname,{SHARED_LIBRARY}"))
        .arg("-o")
        .arg(directory.join(SHARED_LIBRARY))
        .arg(&source)
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "the C implementation must build as a shared library:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_program_against_a_krusty_built_interop_klib_links_and_calls_its_shared_library() {
    let Some(target) = NativeTarget::host()
        .filter(|target| target.arch == Arch::X86_64 && krusty::native::can_link(*target))
    else {
        return;
    };
    let Some(stdlib) = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    ) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "required Native stdlib KLIB is unavailable"
        );
        return;
    };
    let scratch = Scratch::new();
    shared_library(&c_compiler().expect("a host C compiler"), &scratch.0);

    let klib = scratch.0.join("shared_call.klib");
    let built = Command::new(common::krusty_binary())
        .arg("cinterop")
        .arg("-def")
        .arg(shared_fixtures().join("shared_call.def"))
        .arg("-compiler-option")
        .arg(format!("-I{}", shared_fixtures().display()))
        .arg("-linker-option")
        .arg(format!("-L{}", scratch.0.display()))
        .arg("-o")
        .arg(&klib)
        .output()
        .expect("run krusty cinterop");
    assert_eq!(
        (
            built.status.code(),
            String::from_utf8_lossy(&built.stdout).into_owned(),
            String::from_utf8_lossy(&built.stderr).into_owned()
        ),
        (Some(0), String::new(), String::new()),
        "krusty cinterop builds the interop KLIB silently"
    );
    // The implementation is gone before the program is built: the KLIB's link facts are all the
    // link reads.
    let loaded = scratch.0.join("loaded");
    std::fs::create_dir_all(&loaded).expect("create the loader's library directory");
    std::fs::rename(scratch.0.join(SHARED_LIBRARY), loaded.join(SHARED_LIBRARY))
        .expect("move the shared library out of the link's reach");

    let native_inputs = krusty::native::NativeKlibInputs::open(
        &[
            krusty::klib_libraries::KlibLibraryInput::dependency(klib),
            krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
                stdlib,
                krusty::klib_libraries::KlibStdlibSemantics::Native,
            ),
        ],
        target,
    )
    .expect("assemble the interop KLIB and Native stdlib KLIB once");
    let libraries = native_inputs.libraries();
    for (name, expected) in [
        ("krusty_shared_sum", "krusty_shared_sum"),
        ("krusty_shared_scale", "krusty_shared_scale"),
    ] {
        let symbols =
            libraries.symbols(SymbolNamespace::Package(type_name("fixture/sharedc")), name);
        let [function] = symbols.callables.functions() else {
            panic!(
                "expected one `{name}`, found {}",
                symbols.callables.functions().len()
            );
        };
        let realization = libraries
            .external_callable(
                function
                    .callable
                    .external_identity
                    .expect("the declaration has a provider identity"),
            )
            .expect("the provider realizes its assigned identity");
        let Some(krusty::libraries::KlibDeclarationSignature::Public(signature)) =
            &realization.declaration_signature
        else {
            panic!("`{name}` has a public KLIB identity")
        };
        assert_eq!(
            signature.mask(),
            4,
            "`{name}` has a Native interop identity"
        );
        assert_eq!(
            realization
                .direct_c_function
                .as_ref()
                .expect("the provider authenticated the direct C target")
                .symbol(),
            expected
        );
    }

    let (libraries, imports) = native_inputs.into_parts();

    let inputs = [SourceInput::kotlin(SHARED_SOURCE).with_file_stem("sharedCall")];
    let stems = ["sharedCall".to_owned()];
    let mut diags = DiagSink::new();
    let backend = CraneliftBackend::new(target)
        .with_entry(Entry::Box)
        .verified();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(backend.compilation_target(), Box::new(libraries)),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, "shared", &mut diags);
    assert_eq!(diags.render_all(&[("sharedCall.kt", SHARED_SOURCE)]), "");
    let objects = artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();

    assert_eq!(
        imports
            .iter()
            .map(|library| {
                let exports = library.exports().into_iter().map(|(name, _)| name);
                (library.soname(), exports.collect::<Vec<_>>())
            })
            .collect::<Vec<_>>(),
        vec![(
            SHARED_LIBRARY,
            vec!["krusty_shared_scale", "krusty_shared_sum"]
        )]
    );
    let image = krusty::native::link_program_with_libraries(&objects, &imports, target)
        .expect("link the Native program with its imported library");
    let executable = scratch.0.join("program");
    std::fs::write(&executable, image).expect("write the Native program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("make the Native program executable");
    }
    let output = common::run_freshly_written(
        Command::new(&executable)
            .env_clear()
            .env("LD_LIBRARY_PATH", &loaded),
    )
    .expect("run the Native program");
    assert_eq!(
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned()
        ),
        (
            Some(0),
            format!("{}OK", krusty::native::BOX_RESULT_FRAME),
            String::new()
        ),
        "the program calls both C functions through the imported shared library"
    );
}
