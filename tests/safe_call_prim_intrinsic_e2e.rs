//! Primitive bitwise/shift operator-methods (`a.and(b)`, `a shl b`, …) lower to the bit intrinsic
//! through the SHARED `lower_prim_op_method` helper, so both the plain `.` call and an
//! (unnecessary) safe `?.` call on a non-null primitive receiver compile to the same `iand`/`ishl`.
//! Before consolidation the bitwise arm lived only in the `.` waterfall, so `a?.and(b)` was declined.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn dot_path_bitwise_regression() {
    const SRC: &str = "fun box(): String {\n\
    val a = 6; val b = 3\n\
    if (a.and(b) != 2) return \"and\"\n\
    if (a.or(b) != 7) return \"or\"\n\
    if (a.xor(b) != 5) return \"xor\"\n\
    if (a.shl(1) != 12) return \"shl\"\n\
    if (a.shr(1) != 3) return \"shr\"\n\
    if ((-8).ushr(1) != 2147483644) return \"ushr\"\n\
    val l = 6L\n\
    if (l.and(3L) != 2L) return \"land\"\n\
    if (l.shl(1) != 12L) return \"lshl\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("dot-path bitwise compiles + runs"), "OK");
}

#[test]
fn safe_call_bitwise_on_nonnull_primitive() {
    const SRC: &str = "fun box(): String {\n\
    val a = 6; val b = 3\n\
    if (a?.and(b) != 2) return \"and\"\n\
    if (a?.or(b) != 7) return \"or\"\n\
    if (a?.xor(b) != 5) return \"xor\"\n\
    if (a?.shl(1) != 12) return \"shl\"\n\
    val l = 6L\n\
    if (l?.and(3L) != 2L) return \"land\"\n\
    if (l?.shl(1) != 12L) return \"lshl\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("safe-call bitwise on a non-null primitive compiles + runs"),
        "OK"
    );
}
