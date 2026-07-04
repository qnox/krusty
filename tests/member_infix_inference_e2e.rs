//! A member (object/class) function with an *expression body* whose value is a builtin bitwise/shift
//! infix call (`r shl 8`, `a or b`, `n.inv()`) infers its return type from the receiver (`Int`/`Long`),
//! exactly like the equivalent top-level function. Before the fix the lightweight member-signature
//! inference didn't recognize `r.shl(8)` (the infix call's desugaring) and defaulted the return type to
//! `Unit`, rejecting the body with a spurious "expected 'Unit', actual 'Int'". Round-tripped under
//! `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn member_infix_bitwise_expr_body_infers_int() {
    const SRC: &str = "object RGBA {\n\
    fun packFast(r: Int, g: Int, b: Int, a: Int) = (r shl 0) or (g shl 8) or (b shl 16) or (a shl 24)\n\
    fun maskLong(v: Long) = (v and 0xFF) or (v shl 8)\n\
    fun flip(v: Int) = v.inv()\n\
}\n\
fun box(): String {\n\
    if (RGBA.packFast(1, 2, 3, 4) != 0x04030201) return \"fail pack: \" + RGBA.packFast(1, 2, 3, 4)\n\
    if (RGBA.maskLong(2L) != (2L and 0xFF or (2L shl 8))) return \"fail long\"\n\
    if (RGBA.flip(0) != -1) return \"fail inv\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("member infix-bitwise expr body should compile + run");
    assert_eq!(out, "OK");
}
