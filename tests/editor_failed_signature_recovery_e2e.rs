//! One declaration whose signature cannot be finalized (an unresolved return type) must not take
//! the inferred types of every OTHER declaration with it.
//!
//! The editor pipeline checks bodies through the legacy symbol table, and that table receives
//! finalized signatures only by projection. A module with a single genuine error is the state an
//! editor sits in most of the time, so these go through the editor entry point
//! (`analyze_source_set_prefix_with_features`) rather than the batch compiler's streaming pass.

use super::common;
use krusty::jvm::classpath::Classpath;

fn editor_diagnostics(source: &str) -> Vec<String> {
    let cp = std::rc::Rc::new(Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let inputs = [krusty::frontend::SourceInput::kotlin(source)];
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    diags.diags.iter().map(|d| d.msg.clone()).collect()
}

const INFERRED_PROPERTIES: &str = r#"
package sample

private val text = lazy { "x" }
private val builder = StringBuilder().apply { append("y") }
private val counts = mutableMapOf<String, Int>()
private val pattern = Regex("z")

fun useText(): Int = text.value.length
fun useBuilder(): Int = builder.length
fun useCounts(): Int = counts.size
fun usePattern(): Int = pattern.pattern.length
"#;

#[test]
fn inferred_properties_resolve_in_a_valid_module() {
    assert_eq!(
        editor_diagnostics(INFERRED_PROPERTIES),
        Vec::<String>::new()
    );
}

#[test]
fn a_failed_signature_elsewhere_keeps_other_inferred_properties_typed() {
    let source = format!("{INFERRED_PROPERTIES}\nfun broken(): Missing = TODO()\n");
    assert_eq!(
        editor_diagnostics(&source),
        vec!["unresolved reference 'Missing'.".to_string()],
        "a failed signature must not erase the other inferred properties"
    );
}

#[test]
fn a_failed_member_signature_keeps_sibling_members_typed() {
    let source = r#"
package sample

object Holder {
    private val builder = StringBuilder().apply { append("y") }
    fun broken(): Missing = TODO()
    fun useBuilder(): Int = builder.length
}
"#;
    assert_eq!(
        editor_diagnostics(source),
        vec!["unresolved reference 'Missing'.".to_string()],
        "a failed member signature must not erase the sibling inferred property"
    );
}
