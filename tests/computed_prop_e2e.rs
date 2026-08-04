//! Computed properties (custom getter, no backing field): top-level `val x get() = …` → static
//! `getX()`; class `val y get() = …` → instance `getX()` (`obj.y`/unqualified `y`). Round-tripped
//! under `-Xverify:all`.

use super::common;

#[test]
fn computed_properties_run() {
    const SRC: &str = "val top: Int get() = 42\n\
class C(val a: Int, val b: Int) {\n\
    val sum: Int get() = a + b\n\
    val label: String get() = \"v\" + sum\n\
    fun viaThis(): Int = sum\n\
}\n\
fun box(): String {\n\
if (top != 42) return \"f1\"\n\
val c = C(2, 3)\n\
if (c.sum != 5) return \"f2\"\n\
if (c.viaThis() != 5) return \"f3\"\n\
if (c.label != \"v5\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

/// A TOP-LEVEL computed property with NO type annotation infers from its expression getter body —
/// and the body may read another top-level property (`val derived get() = holder.value`).
/// Signature-phase getter inference scoped only context parameters, so the read resolved nothing
/// and the property typed as Error ("cannot infer the type of property 'derived'").
/// The initializer branch already threaded the already-collected top-level props in; the getter
/// branch now shares that scope. A computed property reading an EARLIER computed property works
/// too. Getter bodies are executable, so a LATER property is also legal; a bounded signature retry
/// resolves that forward edge without changing the sequential rules for eager initializers.
#[test]
fn computed_property_getter_reads_toplevel_property() {
    const SRC: &str = "class Holder(val value: String)\n\
val holder = Holder(\"OK\")\n\
val derived get() = holder.value\n\
val repeated get() = derived\n\
val forward get() = forwardAgain\n\
val forwardAgain get() = laterHolder.value\n\
val laterHolder = Holder(\"OK\")\n\
fun box(): String = if (derived == \"OK\" && repeated == \"OK\" && forward == \"OK\") \"OK\" else \"fail: $derived\"\n";
    common::expect_box_ok_with_stdlib(SRC, "G");
}

#[test]
fn computed_property_getter_reads_sibling_file_property() {
    // Signature collection owns a module-wide property table, so the same inference path must work
    // across the per-file streaming boundary; no separate same-file lookup is needed.
    common::expect_box_ok_files_with_stdlib(
        &[
            (
                "shared/State",
                "package shared\nclass State(val value: String)\nval state = State(\"OK\")\n",
            ),
            (
                "shared/Use",
                "package shared\nval computed get() = state.value\nfun box(): String = computed\n",
            ),
        ],
        "computed_prop_sibling",
    );
}
