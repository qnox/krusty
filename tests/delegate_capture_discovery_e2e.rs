//! A source `by` convention and an anonymous object in the same file. Capture discovery
//! rechecks the file before a resolved index exists. The delegate's declared shape lives on
//! that index, so the scratch pass used to panic with "a selected source delegate convention
//! must retain its shape" instead of reading the candidate's own signature.

use super::common;

const SOURCE: &str = "\
interface Marker\n\
\n\
class Del(val value: String) {\n\
    operator fun getValue(thisRef: Any?, property: Any?): String = value\n\
}\n\
\n\
class Receiver(val value: String)\n\
\n\
operator fun Receiver.getValue(thisRef: Any?, property: Any?): String = value\n\
\n\
fun panel(block: () -> Marker): Marker = block()\n\
\n\
class Host {\n\
    val member by Del(\"member\")\n\
    val extension by Receiver(\"extension\")\n\
    fun create(): Marker {\n\
        return panel {\n\
            object : Marker {}\n\
        }\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    val host = Host()\n\
    if (host.member != \"member\") return \"member\"\n\
    if (host.extension != \"extension\") return \"extension\"\n\
    return \"OK\"\n\
}\n\
";

#[test]
fn source_delegate_with_anonymous_object_matches_kotlinc() {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, symbols, info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    assert!(
        !diagnostics.has_errors(),
        "{}",
        diagnostics.render("delegate.kt", SOURCE)
    );
    assert!(symbols.is_some());
    assert!(info.is_some());
    common::expect_box_same_as_kotlinc(SOURCE, "DelegateCaptureDiscovery");
}
