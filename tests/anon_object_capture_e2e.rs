//! `object : I { … }` expressions that capture enclosing parameters/locals — exercises the parser's
//! anonymous-object capture analysis (anon_bound_names / anon_body_uses / anon_body_writes /
//! rewrite_anon_captures), a path the box corpus does not reach.

mod common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn captures_read_parameter() {
    run_ok(
        "AnonRead",
        "interface P { fun get(): Int }\n\
         fun mk(base: Int): P = object : P { override fun get(): Int = base + 1 }\n\
         fun box(): String { val p = mk(41); return if (p.get() == 42) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn captures_multiple_parameters() {
    run_ok(
        "AnonMulti",
        "interface Q { fun sum(): Int }\n\
         fun mk(a: Int, b: Int, c: Int): Q = object : Q { override fun sum(): Int = a + b + c }\n\
         fun box(): String { return if (mk(1, 2, 3).sum() == 6) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn captures_local_val() {
    run_ok(
        "AnonLocal",
        "interface R { fun v(): String }\n\
         fun box(): String {\n\
         val name = \"kt\"\n\
         val r = object : R { override fun v(): String = name + \"!\" }\n\
         return if (r.v() == \"kt!\") \"OK\" else \"F\" }\n",
    );
}

#[test]
fn bound_name_shadows_capture() {
    run_ok(
        "AnonShadow",
        "interface S { fun f(x: Int): Int }\n\
         fun mk(base: Int): S = object : S { override fun f(x: Int): Int = x + base }\n\
         fun box(): String { return if (mk(10).f(5) == 15) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn capture_used_in_property_initializer() {
    run_ok(
        "AnonProp",
        "interface T { fun g(): Int }\n\
         fun mk(seed: Int): T = object : T { val stored = seed * 2; override fun g(): Int = stored }\n\
         fun box(): String { return if (mk(21).g() == 42) \"OK\" else \"F\" }\n",
    );
}
