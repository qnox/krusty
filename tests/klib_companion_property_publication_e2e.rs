//! PLATFORM-SPECIFIC: real Native KLIB artifacts pin companion-block metadata and accessor ABI.
//! The full portable library/consumer runtime control remains in the shared companion-block case.

use krusty::backend::{Artifact, Backend, CheckedIrFile, DependencyMetadata};
use krusty::compilation_target::CompilationTarget;
use krusty::diag::DiagSink;
use krusty::klib::backend::{KlibBackend, KlibModule};
use krusty::klib::write::KlibPlatform;
use super::common;
use krusty::klib::KlibArchive;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use krusty::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use krusty::metadata::id_signature::{
    package_property_accessor_signature, MetadataAccessor, MetadataClass, MetadataContainer,
};
use krusty::metadata::semantic::{parse_package_fragment_checked, KotlinPackage};

const SOURCE: &str = "\
// LANGUAGE: +CompanionBlocksAndExtensions
package published
class Ordinary {
    companion {
        var answer: Int
            get() = 42
            set(value) { if (value < 0) throw IllegalArgumentException(\"negative\") }
    }
}
value class Value(val value: Int) {
    companion { val answer: Int get() = 7 }
}
";

struct ObserveExports {
    backend: KlibBackend,
    expected: Vec<KlibDependencyCallable>,
}

impl Backend for ObserveExports {
    type State = KlibModule;

    fn compilation_target(&self) -> CompilationTarget {
        Backend::compilation_target(&self.backend)
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let exports = self.backend.record_ir_file(&file, state, diags);
        let callables = exports.callables().map(|(_, id)| id).collect::<Vec<_>>();
        for expected in &self.expected {
            assert!(
                callables.contains(&expected),
                "missing accessor export {expected:?}"
            );
        }
        let properties = exports
            .property_declarations()
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        for expected in [&self.expected[0], &self.expected[2]] {
            assert!(
                properties.contains(&expected),
                "missing property export {expected:?}"
            );
        }
        Vec::new()
    }

    fn finalize(&self, state: Self::State, module: &str) -> Vec<Artifact> {
        Backend::finalize(&self.backend, state, module)
    }
}

fn package(path: &std::path::Path) -> KotlinPackage {
    let archive = KlibArchive::open(path).expect("open the actual KLIB artifact");
    let fragments = archive
        .package_fragments()
        .into_iter()
        .filter(|fragment| fragment.package_fqname == "published")
        .collect::<Vec<_>>();
    let [fragment] = fragments.as_slice() else {
        panic!("the whole input has one package fragment")
    };
    parse_package_fragment_checked(&archive.read(&fragment.entry).unwrap()).unwrap()
}

fn companion_accessors(package: &KotlinPackage) -> Vec<KlibDependencyCallable> {
    let namespace = ["published".to_owned()];
    let mut signatures = Vec::new();
    for name in ["published/Ordinary", "published/Value"] {
        let class = &package.classes[name];
        let scope = [MetadataClass::of(name.rsplit('/').next().unwrap(), class)];
        let container = MetadataContainer {
            package: &namespace,
            classes: &scope,
            native_interop_library: false,
        };
        let properties = class
            .properties
            .iter()
            .filter(|p| p.is_static)
            .collect::<Vec<_>>();
        let [property] = properties.as_slice() else {
            panic!("each class declares one companion-block property")
        };
        assert_eq!(property.name, "answer");
        assert!(property.receiver.is_none());
        assert!(property.context_params.is_empty());
        for accessor in [MetadataAccessor::Getter, MetadataAccessor::Setter] {
            if accessor == MetadataAccessor::Setter && !property.is_var {
                continue;
            }
            signatures.push(KlibDependencyCallable {
                module: "companion_properties".into(),
                declaration: KlibDeclarationSignature::Accessor(
                    package_property_accessor_signature(container, property, accessor).unwrap(),
                ),
            });
        }
    }
    assert_eq!(signatures.len(), 3);
    signatures
}

#[test]
fn ordinary_and_value_companion_properties_publish_the_official_accessor_identities() {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").map(std::path::PathBuf::from) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    let work = crate::common::scratch_dir().expect("KLIB property publication directory");
    let source = work.join("Properties.kt");
    std::fs::write(&source, SOURCE).unwrap();
    let official = work.join("official/companion_properties");
    std::fs::create_dir_all(official.parent().unwrap()).unwrap();
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args([
            "-p",
            "library",
            "-nopack",
            "-language-version",
            "2.4",
            "-XXLanguage:+CompanionBlocksAndExtensions",
            "-o",
        ])
        .arg(&official)
        .arg(&source)
        .output()
        .expect("run the exact official Native producer");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = companion_accessors(&package(&official));
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(CompilationTarget::Native).unwrap();
    let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        stdlib.clone(),
        KlibStdlibSemantics::Native,
    )])
    .unwrap();
    let mut diags = DiagSink::new();
    let inputs = [krusty::source::SourceInput::kotlin(SOURCE).with_file_stem("Properties")];
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(
            CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::from_source_in_environment(SOURCE),
        &mut diags,
    );
    let backend = ObserveExports {
        backend: KlibBackend::new(
            KlibPlatform::Native,
            common::lane_klib_stamp(krusty::language_version::LanguageVersion::V2_4),
        ),
        expected: expected.clone(),
    };
    let entries = krusty::compiler::emit_analyzed(
        analysis,
        &["Properties".into()],
        &backend,
        "companion_properties",
        &mut diags,
    );
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|d| d.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    let written = work.join("written/companion_properties");
    for (entry, bytes) in entries {
        let path = written.join(entry);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    assert_eq!(companion_accessors(&package(&written)), expected);
    let reread = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(stdlib, KlibStdlibSemantics::Native),
        KlibLibraryInput::dependency(written),
    ])
    .unwrap();
    use krusty::symbol_source::SymbolSource;
    for expected in expected {
        let (_, declaration) = reread
            .external_callable_by_dependency(&expected)
            .expect("the written KLIB provider exposes the exact accessor declaration");
        assert_eq!(declaration.dependency_callable.as_ref(), Some(&expected));
        assert!(declaration.callable.source_receiver.is_none());
    }
    std::fs::remove_dir_all(work).unwrap();
}
