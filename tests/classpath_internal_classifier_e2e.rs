//! An `internal` CLASSPATH classifier is invisible to another module's source in TYPE positions too —
//! kotlinc rejects `import lib.Hidden` + `fun take(h: Hidden)` against a compiled dependency
//! ("cannot access 'class Hidden': it is internal in file"), not just construction/member access
//! (docs/SPEC.md § classpath visibility). Same-module `internal` classifiers stay usable (the module
//! path, `frontend::inferred_friend_sources_expose_internal_classifiers`), and a PUBLIC sibling from
//! the same jar keeps resolving — the gate must not over-block.

use super::common;

const LIB: &str = "package lib\n\
    internal class Hidden { fun f(): Int = 1 }\n\
    class Pub(val v: String)\n";

#[test]
fn internal_classpath_class_in_type_position_is_rejected() {
    let Some(diagnostics) = common::diagnostics_against(
        "internal_classifier_type_pos",
        LIB,
        "import lib.Hidden\n\
         fun take(h: Hidden): Int = 0\n\
         fun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Hidden")),
        "internal classpath classifier must not resolve as a parameter type (kotlinc: cannot access); got {diagnostics:?}"
    );
}

// NOTE: a package-qualified type WITHOUT an import (`fun take(h: lib.Hidden)`) is not covered here —
// krusty types package-qualified references as silent `Error` even for PUBLIC classes (`lib.Pub`'s
// members don't resolve either), a pre-existing FQ-type gap orthogonal to the visibility gate. Any
// USE of such a value still errors; only an unused parameter compiles where kotlinc rejects.

#[test]
fn public_sibling_class_still_resolves() {
    common::expect_box_ok_against(
        "internal_classifier_pub_sibling",
        LIB,
        "import lib.Pub\n\
         fun take(p: Pub): String = p.v\n\
         fun box(): String = take(Pub(\"OK\"))\n",
    );
}
