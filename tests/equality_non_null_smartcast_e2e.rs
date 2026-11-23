//! `a == b` proves `a` non-null where the equality holds when `b` has a non-null type, and proves
//! nothing when `b` may be null (kotlinc rejects the dereference there).

use super::common;

fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

#[test]
fn equality_with_non_null_operand_smart_casts_the_other_side() {
    const SRC: &str = "class Box(val text: String?)\n\
fun String?.same(other: String): Int = if (this == other) length else -1\n\
fun local(a: String?, b: String): Int = if (a == b) a.length else -1\n\
fun reversed(a: String?, b: String): Int = if (b == a) a.length else -1\n\
fun property(box: Box, b: String): Int = if (box.text == b) box.text.length else -1\n";
    assert_eq!(diags(SRC), Vec::<String>::new());
}

#[test]
fn equality_with_nullable_operand_proves_nothing() {
    const SRC: &str = "fun local(a: String?, b: String?): Int = if (a == b) a.length else -1\n";
    assert_eq!(
        diags(SRC),
        vec![
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        ]
    );
}

#[test]
fn inequality_does_not_smart_cast_the_then_branch() {
    const SRC: &str = "fun local(a: String?, b: String): Int = if (a != b) a.length else -1\n";
    assert_eq!(
        diags(SRC),
        vec![
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        ]
    );
}
