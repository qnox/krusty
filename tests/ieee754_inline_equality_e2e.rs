//! An inline `==` keeps the equality mode the checker selected from the static operand types.
//!
//! `T : Comparable<Double>` is structural, so substituting `Double` must not turn it into IEEE
//! (`-0.0 == 0.0` stays false). A static `Double` parameter stays IEEE (`-0.0 == 0.0` is true).
//! Ordering on `Comparable` stays the total order (`-0.0 < 0.0` is true).
use super::common;

const LIB: &str = "\
inline fun less(a: Comparable<Double>, b: Double): Boolean = a < b\n\
inline fun equals(a: Comparable<Double>, b: Comparable<Double>): Boolean = a == b\n\
inline fun <T : Comparable<Double>> lessGeneric(a: T, b: Double): Boolean = a < b\n\
inline fun <T : Comparable<Double>> equalsGeneric(a: T, b: Double): Boolean = a == b\n\
inline fun <reified T : Comparable<Double>> lessReified(a: T, b: Double): Boolean = a < b\n\
inline fun <reified T : Comparable<Double>> equalsReified(a: T, b: T): Boolean = a == b\n\
inline fun less754(a: Double, b: Double): Boolean = a < b\n\
inline fun equals754(a: Double, b: Double): Boolean = a == b\n\
";

const MAIN: &str = "\
fun box(): String {\n\
    if (!less(-0.0, 0.0)) return \"fail 1\"\n\
    if (equals(-0.0, 0.0)) return \"fail 2\"\n\
    if (!lessGeneric(-0.0, 0.0)) return \"fail 3\"\n\
    if (equalsGeneric(-0.0, 0.0)) return \"fail 4\"\n\
    if (!lessReified(-0.0, 0.0)) return \"fail 5\"\n\
    if (equalsReified(-0.0, 0.0)) return \"fail 6\"\n\
    if (less754(-0.0, 0.0)) return \"fail 7\"\n\
    if (!equals754(-0.0, 0.0)) return \"fail 8\"\n\
    return \"OK\"\n\
}\n\
";

#[test]
fn inline_comparable_equality_stays_structural_at_runtime() {
    common::expect_box_ok_files_with_stdlib(
        &[("lib.kt", LIB), ("main.kt", MAIN)],
        "ieee754/inline",
    );
}

/// The mode on the checked equality is still the one in the expanded call, after `T` is `Double`.
#[test]
fn inlined_equality_keeps_the_checked_mode() {
    let source = format!(
        "{LIB}\n\
         fun structuralUse(a: Double): Boolean = equalsGeneric(a, 0.0)\n\
         fun ieeeUse(a: Double): Boolean = equals754(a, 0.0)\n"
    );
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let (files, diagnostics) = common::capture_common_ir(&source, "InlineEqualityMode", platform);
    assert!(diagnostics.is_empty(), "frontend rejected: {diagnostics:?}");
    let file = files.into_iter().next().expect("one lowered file");
    let equality_mode = |function_name: &str| {
        let body = file
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .and_then(|function| function.body)
            .unwrap_or_else(|| panic!("{function_name} has a body"));
        let mut equalities = Vec::new();
        let mut stack = vec![body];
        while let Some(expression) = stack.pop() {
            if let krusty::ir::IrExpr::Equality { mode, op, .. } = file.expr(expression) {
                equalities.push((*op, *mode));
            }
            krusty::ir::for_each_child(&file.exprs, expression, &mut |child| stack.push(child));
        }
        let [(op, mode)] = equalities.as_slice() else {
            panic!("expected exactly one equality in {function_name}, found {equalities:?}")
        };
        assert_eq!(*op, krusty::ir::IrBinOp::Eq);
        *mode
    };
    assert_eq!(
        [
            ("structuralUse", equality_mode("structuralUse")),
            ("ieeeUse", equality_mode("ieeeUse")),
        ],
        [
            ("structuralUse", krusty::ir::EqualityMode::Structural),
            ("ieeeUse", krusty::ir::EqualityMode::Ieee754),
        ],
        "each inline call must keep its own checked equality mode"
    );
}
