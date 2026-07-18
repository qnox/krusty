//! Same-file top-level `$default` calls need one mask word per 32 parameters. The backend already emits
//! multi-mask facade stubs; this test covers the lowering/emitter call path for an omitted non-constant
//! default past parameter 31.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn toplevel_default_call_uses_second_mask_word() {
    const SRC: &str = "class Box(val n: Int)\n\
        fun f(p0: Int, p1: Int, p2: Int, p3: Int, p4: Int, p5: Int, p6: Int, p7: Int,\n\
        \x20 p8: Int, p9: Int, p10: Int, p11: Int, p12: Int, p13: Int, p14: Int, p15: Int,\n\
        \x20 p16: Int, p17: Int, p18: Int, p19: Int, p20: Int, p21: Int, p22: Int, p23: Int,\n\
        \x20 p24: Int, p25: Int, p26: Int, p27: Int, p28: Int, p29: Int, p30: Int, p31: Int,\n\
        \x20 p32: Int, p33: Box = Box(34)): Int = p0 + p32 + p33.n\n\
        fun box(): String = if (f(1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,\n\
        \x20 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2) == 37) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("top-level multi-mask default call"), "OK");
}
