//! The headers krusty serves a KSP processor for classpath classifiers, against what KSP2 logs for
//! them (`tests/fixtures/ksp/library.expected`).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use krusty::diag::DiagSink;
use krusty::features::LangFeatures;
use krusty::frontend::{
    analyze_source_set_prefix_with_features, PlatformProvider, SourceSetAnalysis,
};
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::ksp_round::KspLibraries;
use krusty::source::SourceInput;
use krusty_plugin_ir::{
    KspAnnotation, KspClassKind, KspClasspath, KspLibraryClass, KspOrigin, KspType,
    KspTypeArgument, KspTypeDeclaration, KspValue, KspVariance,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ksp")
}

fn platform(paths: Vec<PathBuf>) -> PlatformProvider {
    let classpath = Rc::new(Classpath::new(paths));
    let libraries = JvmLibraries::new(classpath).expect("the JVM provider");
    PlatformProvider::jvm(Box::new(libraries))
}

/// The library fixture's dependency, compiled by krusty into a classpath directory.
fn dependency(paths: &[PathBuf]) -> PathBuf {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ksp-library-classes-dependency");
    let _ = std::fs::remove_dir_all(&out);
    let text = std::fs::read_to_string(fixtures().join("dependency/Dependency.kt")).unwrap();
    let inputs = [SourceInput::kotlin(&text).with_file_stem("Dependency")];
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        platform(paths.to_vec()),
        &LangFeatures::from_source(&text),
        &mut diagnostics,
    );
    let backend = krusty::jvm::JvmBackend::new(Rc::new(Classpath::new(paths.to_vec())));
    let artifacts = krusty::compiler::emit_analyzed(
        analysis,
        &["Dependency".to_string()],
        &backend,
        "main",
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "krusty refused Dependency.kt");
    for (path, bytes) in artifacts {
        let file = out.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    out
}

fn analysis() -> Option<SourceSetAnalysis> {
    krusty::toolchain::stdlib_jar()?;
    let paths: Vec<PathBuf> = krusty::toolchain::stdlib_jar()
        .into_iter()
        .chain(krusty::toolchain::jdk_modules())
        .collect();
    let mut classpath = paths.clone();
    classpath.push(dependency(&paths));
    let inputs = [SourceInput::kotlin("package library\n")];
    let mut diagnostics = DiagSink::new();
    Some(analyze_source_set_prefix_with_features(
        &inputs,
        0,
        inputs.len(),
        platform(classpath),
        &LangFeatures::new(),
        &mut diagnostics,
    ))
}

fn render(ty: &KspType) -> String {
    let mut out = match &ty.declaration {
        KspTypeDeclaration::Class(name) => name.clone(),
        KspTypeDeclaration::TypeAlias { qualified_name, .. } => qualified_name.clone(),
        KspTypeDeclaration::TypeParameter { owner, name, .. } => format!("{owner}.{name}"),
    };
    if !ty.arguments.is_empty() {
        let arguments: Vec<String> = ty
            .arguments
            .iter()
            .map(|argument| match argument {
                KspTypeArgument::Star => "*".to_string(),
                KspTypeArgument::Invariant(ty) => render(ty),
                KspTypeArgument::Out(ty) => format!("out {}", render(ty)),
                KspTypeArgument::In(ty) => format!("in {}", render(ty)),
            })
            .collect();
        out.push_str(&format!("<{}>", arguments.join(", ")));
    }
    if ty.nullable {
        out.push('?');
    }
    out
}

/// An annotation of a library classifier in the form the library fixture's processor logs it: with
/// the classifier's origin, or the compiler's for a default nested in one.
fn logged_annotation(annotation: &KspAnnotation, origin: &str) -> String {
    let origin = if annotation.synthetic {
        "SYNTHETIC"
    } else {
        origin
    };
    let arguments: Vec<String> = annotation
        .arguments
        .iter()
        .flat_map(|arguments| &arguments.arguments)
        .map(|argument| {
            format!(
                "{}={}",
                argument.name,
                logged_value(&argument.value, origin)
            )
        })
        .collect();
    format!(
        "@{}={} origin={origin} ({})",
        annotation.short_name,
        annotation.qualified_name,
        arguments.join(", ")
    )
}

fn logged_value(value: &KspValue, origin: &str) -> String {
    match value {
        KspValue::Boolean(value) => format!("{value}:Boolean"),
        KspValue::Byte(value) => format!("{value}:Byte"),
        KspValue::Short(value) => format!("{value}:Short"),
        KspValue::Int(value) => format!("{value}:Integer"),
        KspValue::Long(value) => format!("{value}:Long"),
        KspValue::Char(value) => {
            format!("{}:Character", char::from_u32(u32::from(*value)).unwrap())
        }
        KspValue::Float(bits) => format!("{:?}:Float", f32::from_bits(*bits)),
        KspValue::Double(bits) => format!("{:?}:Double", f64::from_bits(*bits)),
        KspValue::String(value) if value.contains('\n') => "multi-line:String".to_string(),
        KspValue::String(value) => format!("\"{value}\":String"),
        KspValue::EnumEntry { class, entry } => format!("entry ENUM_ENTRY {class}.{entry}"),
        KspValue::Type(ty) => match &ty.declaration {
            KspTypeDeclaration::Class(name) => format!("type {name}"),
            KspTypeDeclaration::TypeAlias { qualified_name, .. } => {
                format!("type {qualified_name}")
            }
            KspTypeDeclaration::TypeParameter { name, .. } => format!("type {name}"),
        },
        KspValue::Annotation(annotation) => logged_annotation(annotation, origin),
        KspValue::Array(values) => {
            let values: Vec<String> = values
                .iter()
                .map(|value| logged_value(value, origin))
                .collect();
            format!("[{}]:List", values.join(", "))
        }
    }
}

/// `class` in the form the library fixture's processor logs it.
fn logged(name: &str, library: Option<KspLibraryClass>) -> String {
    let Some(library) = library else {
        return format!("{name} null");
    };
    let class = &library.class;
    let kind = match class.kind {
        KspClassKind::Interface => "INTERFACE",
        KspClassKind::Class => "CLASS",
        KspClassKind::EnumClass => "ENUM_CLASS",
        KspClassKind::EnumEntry => "ENUM_ENTRY",
        KspClassKind::Object => "OBJECT",
        KspClassKind::AnnotationClass => "ANNOTATION_CLASS",
    };
    let mut modifiers: Vec<&str> = class
        .modifiers
        .iter()
        .flatten()
        .map(|modifier| modifier.name())
        .collect();
    modifiers.sort_unstable();
    let parameters: Vec<String> = class
        .type_parameters
        .iter()
        .flatten()
        .map(|parameter| {
            let variance = match parameter.variance {
                KspVariance::Invariant => "INVARIANT",
                KspVariance::Out => "COVARIANT",
                KspVariance::In => "CONTRAVARIANT",
            };
            let bounds: Vec<String> = parameter.bounds.iter().map(render).collect();
            format!(
                "{} variance={variance} bounds={}",
                parameter.name,
                bounds.join(" & ")
            )
        })
        .collect();
    let super_types: Vec<String> = class.super_types.iter().flatten().map(render).collect();
    let origin = match library.origin {
        KspOrigin::KotlinLib => "KOTLIN_LIB",
        KspOrigin::JavaLib => "JAVA_LIB",
    };
    // The host refuses the annotations of a declaration whose annotations krusty cannot read.
    let annotations: Vec<String> = match library.annotations_complete {
        true => class
            .annotations
            .iter()
            .map(|annotation| logged_annotation(annotation, origin))
            .collect(),
        false => vec!["<refused>".to_string()],
    };
    format!(
        "{name} -> {kind} {} simple={} package={} parent={} origin={} modifiers={} \
         typeParameters=[{}] supertypes={} annotations={}",
        class.qualified_name,
        class.simple_name,
        library.package_name,
        library.parent.as_deref().unwrap_or("null"),
        origin,
        modifiers.join(","),
        parameters.join("; "),
        super_types.join(", "),
        annotations.join("; "),
    )
}

#[test]
fn library_classes_are_the_headers_ksp2_reports() {
    let Some(analysis) = analysis() else {
        return;
    };
    let libraries = KspLibraries::new(&analysis);
    let expected = std::fs::read_to_string(fixtures().join("library.expected")).unwrap();
    let mut mismatches = Vec::new();
    for line in expected.lines() {
        let line = line.trim_start_matches("w: [ksp] ");
        // The lines after the names show source supertypes, which the host test covers.
        let Some((name, _)) = line.split_once(' ') else {
            continue;
        };
        if line.starts_with('#') || !name.contains('.') {
            continue;
        }
        let actual = logged(name, libraries.class(name));
        if actual != line {
            mismatches.push(format!("want {line}\n got {actual}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
