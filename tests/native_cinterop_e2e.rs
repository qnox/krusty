//! A real cinterop KLIB declaration selected through the Native KLIB provider and called with the
//! C ABI. The fixture is metadata-only; its implementation is a separate C object, so this test
//! covers no cinterop generator or library-discovery path.

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

fn libraries() -> Option<krusty::klib_libraries::KlibLibraries> {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    )?;
    Some(
        krusty::klib_libraries::KlibLibraries::open(&[
            krusty::klib_libraries::KlibLibraryInput::dependency(fixture()),
            krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
                stdlib,
                krusty::klib_libraries::KlibStdlibSemantics::Native,
            ),
        ])
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
    let Some(libraries) = libraries() else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "required Native stdlib KLIB is unavailable"
        );
        return;
    };

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
    let Some(libraries) = libraries() else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "required Native stdlib KLIB is unavailable"
        );
        return;
    };

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
