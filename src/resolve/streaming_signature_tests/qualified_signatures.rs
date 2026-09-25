//! Pass-one coverage for qualified classifier paths.

use super::{DiagSink, LangFeatures, SourceInput, Ty};

fn analyze_jvm_source(
    source: &str,
    stem: &str,
    diagnostics: &mut DiagSink,
) -> crate::frontend::SourceSetAnalysis {
    let inputs = [SourceInput::kotlin(source).with_file_stem(stem)];
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(classpath),
            ))
            .expect("JVM provider initialization"),
        ),
        &LangFeatures::new(),
        diagnostics,
    )
}

#[test]
fn inferred_signature_resolves_a_package_qualified_nested_constructor() {
    let source = r#"
package outerpkg
class Outer {
    class Nested {
        val first = "O"
        val second = "K"
    }
}
fun box() = outerpkg.Outer.Nested().first + Outer.Nested().second
"#;
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_jvm_source(source, "QualifiedNestedSignature", &mut diagnostics);

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("package-qualified nested construction must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved box signature");
    assert_eq!(result.result.get(), Ty::String);
}

/// A default-imported classifier (`java.lang.Package`) outranks the same-named source package, so
/// the next segment is the miss and resolution never backtracks to the package.
#[test]
fn inferred_signature_commits_a_classifier_root_over_a_same_named_package() {
    let source = r#"
package Package
class Outer {
    class Nested {
        val first = "O"
    }
}
fun box() = Package.Outer.Nested().first
"#;
    let mut diagnostics = DiagSink::new();
    analyze_jvm_source(source, "QualifiedRootClash", &mut diagnostics);

    let outer = source.find("Package.Outer").expect("qualified read") + "Package.".len();
    let reported = diagnostics
        .diags
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.span.lo as usize..diagnostic.span.hi as usize,
                diagnostic.msg.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reported,
        [(
            outer..outer + "Outer".len(),
            "unresolved reference 'Outer'."
        )]
    );
}
