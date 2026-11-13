//! One declaration whose signature cannot be finalized (an unresolved return type) must not take
//! the inferred types of every OTHER declaration with it.
//!
//! The editor pipeline checks bodies against the partial finalized declaration index, while its
//! source symbol projection retains successfully finalized signatures. A module with a single
//! genuine error is the state an editor sits in most of the time, so these go through the editor
//! entry point (`analyze_source_set_prefix_with_features`) rather than the batch compiler's
//! streaming pass.

use super::common;
use krusty::jvm::classpath::Classpath;

fn editor_diagnostics(source: &str) -> Vec<String> {
    let cp = std::rc::Rc::new(Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let inputs = [krusty::frontend::SourceInput::kotlin(source)];
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        krusty::frontend::PlatformProvider::jvm(platform),
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

/// A half-typed supertype is the everyday state of a file in an editor. The failure branch now
/// projects the finalized signatures (so the rest of the module keeps its types), and the
/// projection must treat a parent that never resolved as absent rather than as an invariant
/// violation: the analysis worker died with a panic (exit 101) on every such file.
#[test]
fn an_unresolved_supertype_keeps_the_editor_analysis_alive() {
    let source = r#"
package sample

class Child<T> : Missing()
interface Marker : AlsoMissing
class Both : Missing(), AlsoMissing

private val text = lazy { "x" }
fun useText(): Int = text.value.length
"#;
    let d = editor_diagnostics(source);
    assert_eq!(
        d,
        vec![
            "unresolved reference 'Missing'.".to_string(),
            "unresolved reference 'AlsoMissing'.".to_string(),
            "unresolved reference 'AlsoMissing'.".to_string(),
            "unresolved reference 'Missing'.".to_string(),
        ],
        "the unresolved supertypes must be reported exactly without erasing unrelated types"
    );
}

/// A class literal of the class being declared (generic or not), written inside the arguments of its own
/// unresolved superclass constructor call, names a class whose signature never finalized. The
/// literal cannot be star-projected without the class's signature, and the unresolved reference is
/// the whole report; the analysis worker used to die with a panic (exit 101) on this shape.
#[test]
fn a_class_literal_of_a_class_with_an_unresolved_supertype_is_reported_once() {
    let source = r#"
package sample

import kotlin.reflect.KClass

fun use(type: KClass<*>, label: String) {}

class Foo : Missing(
    {
        use(Foo::class, "x")
    }
)

class Generic<T> : AlsoMissing(
    {
        use(Generic::class, "y")
    }
)
"#;
    assert_eq!(
        editor_diagnostics(source),
        vec![
            "unresolved reference 'Missing'.".to_string(),
            "unresolved reference 'AlsoMissing'.".to_string(),
        ],
    );
}

/// `Child::class` names a classifier whose signature did not finalize. kotlinc reports the
/// unresolved supertype and keeps the literal. The editor path must report those messages and
/// must not abort the module.
#[test]
fn a_class_literal_of_a_class_with_an_unresolved_supertype_matches_kotlinc() {
    let source = "\
package sample

class Child<T> : Missing()
fun useClass() = Child::class
";
    common::assert_errors_match_kotlinc(&[("ClassLiteral.kt", source)], &[]);
    let expected = common::reference_error_messages("ClassLiteral", source);
    assert_eq!(editor_diagnostics(source), expected);
}

/// The same recovery for a nested class. The literal's selected declaration is that nested
/// class, not a top-level class whose simple name happens to match the last segment.
#[test]
fn a_class_literal_of_a_nested_class_with_an_unresolved_supertype_matches_kotlinc() {
    let source = "\
package sample

class Outer {
    class Inner<T> : Missing()
}
fun useClass() = Outer.Inner::class
";
    common::assert_errors_match_kotlinc(&[("NestedClassLiteral.kt", source)], &[]);
    let expected = common::reference_error_messages("NestedClassLiteral", source);
    assert_eq!(editor_diagnostics(source), expected);
}

/// A file's own policies still govern its recovery diagnostics. With `broken`'s inferred result
/// unresolved, the compiler reports from the partial index; `@file:Suppress` must still silence the
/// internal-annotation access it covers, so kotlinc's single unresolved reference is the complete
/// output.
#[test]
fn file_level_suppression_survives_a_failed_signature() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Policy.kt",
            "@file:Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
             \n\
             import kotlin.internal.LowPriorityInOverloadResolution\n\
             \n\
             @LowPriorityInOverloadResolution\n\
             fun pick(value: Any): Int = 1\n\
             \n\
             fun broken() = missing()\n",
        )],
        &[],
    );
}
