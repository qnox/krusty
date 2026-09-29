//! An uninitialized local of a type parameter keeps that parameter's erased JVM slot
//! after the inline call specializes the parameter to a primitive.
//!
//! `var result: R` is lowered as a store of `R`'s zero. Unbounded `R` erases to `Object`,
//! so the zero is `null` and a later `Int` is boxed into the slot; the caller's `Int`
//! return unboxes it. A primitive bound (`R : Int`) is already an `int`, and its zero is
//! `0`. Specializing the unbounded local itself to `int` unboxes the `null`.

use super::common;

const UNBOUNDED: &str = "\
inline fun <R> f(size: Int, block: () -> R): R {\n\
    var result: R\n\
    while (true) {\n\
        result = block()\n\
        if (size == 0) break\n\
    }\n\
    return result\n\
}\n\
fun computeResult(size: Int) = f(size) { 42 }\n\
fun box() = if (computeResult(0) == 42) \"OK\" else \"FAIL\"\n\
";

const PRIMITIVE_BOUND: &str = "\
inline fun <R : Int> f(block: () -> R): R {\n\
    var result: R\n\
    result = block()\n\
    return result\n\
}\n\
fun computeResult() = f { 42 }\n\
fun box() = if (computeResult() == 42) \"OK\" else \"FAIL\"\n\
";

/// Value operations, without the inliner's depth markers.
///
/// kotlinc opens an inlined function with `iconst_0; istore` into `$i$f$…` and an
/// inlined lambda with another into `$i$a$…`, plus a `nop` between the local's
/// null store and the lambda marker. Those locals occupy slots, so the value
/// locals are numbered differently. They are not the representation of `result`.
/// Local slots and branch targets are erased; calls stay.
fn value_operations(instructions: &[String]) -> Vec<String> {
    let mut operations = Vec::new();
    let mut pending_zero = false;
    for instruction in instructions {
        let code = instruction
            .split_once(": ")
            .map(|(_, rest)| rest)
            .unwrap_or(instruction);
        if code == "nop" {
            continue;
        }
        if code == "iconst_0" {
            pending_zero = true;
            continue;
        }
        if pending_zero && (code.starts_with("istore_") || code.starts_with("istore ")) {
            pending_zero = false;
            continue;
        }
        if pending_zero {
            operations.push("iconst_0".to_string());
            pending_zero = false;
        }
        operations.push(normalize_local(code));
    }
    if pending_zero {
        operations.push("iconst_0".to_string());
    }
    operations
}

fn normalize_local(code: &str) -> String {
    let opcode = code.split_whitespace().next().unwrap_or(code);
    let bare = opcode.split('_').next().unwrap_or(opcode);
    match bare {
        "iload" | "istore" | "aload" | "astore" | "lload" | "lstore" | "ifne" | "ifeq" | "goto" => {
            bare.to_string()
        }
        _ => code.to_string(),
    }
}

fn assert_same_value_operations(stem: &str, src: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        stem,
        src,
        &format!("{stem}Kt"),
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    let krusty = common::method_instructions(&built.krusty, member);
    assert!(!reference.is_empty(), "kotlinc emits {member}");
    assert_eq!(
        value_operations(&krusty),
        value_operations(&reference),
        "{member}\nkrusty: {krusty:?}\nkotlinc: {reference:?}"
    );
}

#[test]
fn an_uninitialized_unbounded_local_stays_boxed_like_kotlinc() {
    assert_same_value_operations("DeferredGenericLocal", UNBOUNDED, "int computeResult(int);");
}

#[test]
fn an_uninitialized_primitive_bound_local_stays_unboxed_like_kotlinc() {
    assert_same_value_operations(
        "DeferredPrimitiveBoundLocal",
        PRIMITIVE_BOUND,
        "int computeResult();",
    );
}

#[test]
fn an_uninitialized_generic_local_runs() {
    common::expect_box_ok_with_stdlib(UNBOUNDED, "DeferredGenericLocal");
    common::expect_box_ok_with_stdlib(PRIMITIVE_BOUND, "DeferredPrimitiveBoundLocal");
}
