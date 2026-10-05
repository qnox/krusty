//! An inlined type-parameter `var` captured by an escaping lambda keeps the erased holder.
//!
//! The lambda's implementation is shared: a non-reified parameter is not cloned, so its capture
//! parameter stays `Ref$ObjectRef`. Specializing the copied cell to `Ref$IntRef` makes the
//! invokedynamic pass an `IntRef` where that parameter expects an `ObjectRef`.

use super::common;

fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

#[test]
fn inlined_type_parameter_cell_matches_the_erased_lambda() {
    // `apply` is not inline, so the lambda escapes and keeps the shared implementation.
    // A local that is only invoked would be spliced and would not take that holder.
    both_compilers_box(
        r#"
        fun apply(block: () -> Unit) { block() }

        inline fun <T> keep(value: T, next: T): T {
            var acc = value
            apply { acc = next }
            return acc
        }

        fun box(): String {
            var n = 1
            apply { n = n + 6 }
            if (n != 7) return "FAIL n=" + n
            val kept = keep(1, 7)
            if (kept != 7) return "FAIL kept=" + kept
            return "OK"
        }
        "#,
        "erased-capture",
    );
}
