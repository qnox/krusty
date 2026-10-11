use super::*;
use crate::backend::{Backend, CheckedIrFile};
use crate::compilation_target::CompilationTarget;
use crate::features::LangFeatures;
use crate::klib::backend::KlibBackend;
use crate::klib::write::{KlibPlatform, KlibStamp};
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

struct RecordingTarget;

impl Backend for RecordingTarget {
    type State = Vec<String>;

    fn compilation_target(&self) -> CompilationTarget {
        CompilationTarget::Native
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        for declaration in &file.ir.package_functions {
            assert!(
                file.dependency_exports
                    .callable(declaration.function)
                    .is_some(),
                "target code receives the exact KLIB identity exported for its callable"
            );
        }
        state.push(file.module_name.to_string());
        let stem = &file.stems[file.source.raw() as usize];
        vec![(
            format!("{}/{stem}.o", file.module_name),
            file.module_name.as_bytes().to_vec(),
        )]
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        vec![(
            format!("{module_name}.program"),
            state.join(",").into_bytes(),
        )]
    }
}

fn analysis(source: &str, stem: &str, diags: &mut DiagSink) -> StreamingSourceSetAnalysis {
    let inputs = [SourceInput::kotlin(source).with_file_stem(stem)];
    crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::new(
            CompilationTarget::Native,
            Box::new(EmptySymbolSource),
        ),
        &LangFeatures::new(),
        diags,
    )
    .into()
}

fn metadata() -> KlibBackend {
    KlibBackend::new(
        KlibPlatform::Native,
        KlibStamp::new(
            crate::kotlin_version::KotlinVersion::V2_4_20,
            crate::language_version::LanguageVersion::V2_4,
        ),
    )
}

fn manifest(module: &EmittedDependencyModule) -> &str {
    let bytes = module
        .artifacts()
        .iter()
        .find_map(|(path, bytes)| (path == "default/manifest").then_some(bytes))
        .expect("a KLIB sidecar carries its manifest");
    std::str::from_utf8(bytes).expect("a KLIB manifest is UTF-8")
}

#[test]
fn separately_analyzed_modules_publish_distinct_metadata_and_share_target_state() {
    let backend = RecordingTarget;
    let mut session = EmissionSession::new(&backend);

    let mut lib_diags = DiagSink::new();
    let lib = session
        .emit_module(
            analysis(
                "package demo\nfun answer(value: Int): Int = value",
                "Lib",
                &mut lib_diags,
            ),
            &["Lib".to_string()],
            &metadata(),
            "lib",
            &mut lib_diags,
        )
        .expect("lib emits");
    assert_eq!(lib_diags.diags, []);
    assert_eq!(lib.name(), "lib");
    assert!(manifest(&lib).contains("unique_name=lib\n"));

    let mut app_diags = DiagSink::new();
    let app = session
        .emit_module(
            analysis(
                "package demo\nfun box(): String = \"OK\"",
                "App",
                &mut app_diags,
            ),
            &["App".to_string()],
            &metadata(),
            "app",
            &mut app_diags,
        )
        .expect("app emits");
    assert_eq!(app_diags.diags, []);
    assert_eq!(app.name(), "app");
    assert!(manifest(&app).contains("unique_name=app\n"));

    let mut finish_diags = DiagSink::new();
    let target = session.finish("program", &mut finish_diags);
    assert_eq!(finish_diags.diags, []);
    assert_eq!(
        target,
        [
            ("lib/Lib.o".to_string(), b"lib".to_vec()),
            ("app/App.o".to_string(), b"app".to_vec()),
            ("program.program".to_string(), b"lib,app".to_vec()),
        ]
    );
}
