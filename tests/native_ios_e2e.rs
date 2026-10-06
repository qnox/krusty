//! The iOS simulator image.
//!
//! krusty cross-compiles Kotlin to an unsigned arm64 Mach-O `MH_DYLIB`. The compile path does not
//! read an Apple SDK and does not sign the image. This host checks the object format and the load
//! commands dyld would read; it does not execute the dylib.

use object::{Object, ObjectSymbol};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

fn simulator() -> Option<NativeTarget> {
    let target = "ios-simulator-aarch64"
        .parse::<NativeTarget>()
        .expect("the simulator is a supported target");
    krusty::native::can_link(target).then_some(target)
}

fn compile(source: &str, target: NativeTarget) -> (Vec<Artifact>, Vec<String>) {
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Value")];
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = CraneliftBackend::new(target);
    let artifacts = krusty::compiler::emit_analyzed(
        analysis,
        &["Value".to_string()],
        &backend,
        "demo",
        &mut diags,
    );
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

fn objects(artifacts: &[Artifact]) -> Vec<&[u8]> {
    artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect()
}

/// A public function lowered for the simulator links into an unsigned arm64 dylib that imports
/// libSystem and names the function. Nothing here runs the image.
#[test]
fn a_public_function_links_to_an_unsigned_simulator_dylib() {
    if krusty::toolchain::stdlib_jar().is_none() {
        eprintln!("skipping: needs the Kotlin stdlib");
        return;
    }
    let Some(target) = simulator() else {
        eprintln!("skipping: this build has no iOS simulator runtime");
        return;
    };
    let (artifacts, diagnostics) =
        compile("package demo\nfun value(n: Int): Int = n + 1\n", target);
    assert!(
        diagnostics.is_empty(),
        "the simulator backend rejected the program: {diagnostics:?}"
    );
    let objects = objects(&artifacts);
    assert!(!objects.is_empty(), "the backend emitted no object");
    let bytes = krusty::native::link_program(&objects, target)
        .unwrap_or_else(|error| panic!("linking for {target} must succeed: {error}"));
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    assert_eq!(&bytes[..4], &[0xcf, 0xfa, 0xed, 0xfe], "MH_MAGIC_64");
    assert_eq!(u32_at(4), 0x0100_000c, "CPU_TYPE_ARM64");
    assert_eq!(u32_at(12), 6, "MH_DYLIB");
    let ncmds = u32_at(16);
    let mut at = 32usize;
    let mut load_dylib = None;
    let mut build_version = None;
    let mut signed = false;
    let mut bitcode = false;
    for _ in 0..ncmds {
        let cmd = u32_at(at);
        let size = u32_at(at + 4) as usize;
        if cmd == 0x1d {
            signed = true;
        }
        if cmd == 0xc {
            let name_at = at + u32_at(at + 8) as usize;
            let end = bytes[name_at..]
                .iter()
                .position(|&byte| byte == 0)
                .expect("a terminated dylib path");
            load_dylib = Some(String::from_utf8_lossy(&bytes[name_at..name_at + end]).into_owned());
        }
        if cmd == 0x32 {
            build_version = Some((u32_at(at + 8), u32_at(at + 12)));
        }
        if cmd == 0x19 {
            let nsects = u32_at(at + 64) as usize;
            let mut section = at + 72;
            for _ in 0..nsects {
                let name_end = bytes[section..section + 16]
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(16);
                if &bytes[section..section + name_end] == b"__LLVM" {
                    bitcode = true;
                }
                section += 80;
            }
        }
        at += size;
    }
    assert_eq!(load_dylib.as_deref(), Some("/usr/lib/libSystem.B.dylib"));
    assert_eq!(
        build_version,
        Some((7, 15 << 16)),
        "PLATFORM_IOSSIMULATOR, 15.0.0"
    );
    assert!(!signed, "the dylib is unsigned");
    assert!(!bitcode, "the dylib contains no bitcode");

    let file = object::File::parse(bytes.as_slice()).expect("the dylib parses");
    let mut undefined = Vec::new();
    let mut defined = Vec::new();
    for symbol in file.symbols() {
        let Ok(name) = symbol.name() else {
            continue;
        };
        if symbol.is_undefined() {
            undefined.push(name.strip_prefix('_').unwrap_or(name).to_string());
        } else if symbol.scope() == object::SymbolScope::Dynamic {
            defined.push(name.to_string());
        }
    }
    undefined.sort();
    undefined.dedup();
    assert_eq!(undefined, ["exit", "mmap", "munmap", "write"]);
    assert!(
        defined.iter().any(|name| name == "_demo_value__Int"),
        "the public function is exported: {defined:?}"
    );
}
