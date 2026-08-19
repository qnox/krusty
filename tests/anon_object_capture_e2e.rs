use super::common;

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
fn captures_constructor_initialized_local() {
    run_ok(
        "AnonCtorLocal",
        "interface W { fun put(s: String) }\n\
         fun box(): String {\n\
         val sb = StringBuilder()\n\
         val w = object : W { override fun put(s: String) { sb.append(s) } }\n\
         w.put(\"O\")\n\
         w.put(\"K\")\n\
         return sb.toString() }\n",
    );
}

#[test]
fn captures_capitalized_function_result() {
    let library = "package lib\n\
        class Token(val value: String)\n\
        fun Token(value: Int): String = if (value == 1) \"OK\" else \"FAIL\"\n";
    let source = "import lib.Token\n\
        interface Text { fun read(): String }\n\
        fun box(): String {\n\
        val value = Token(1)\n\
        val text = object : Text { override fun read(): String = value }\n\
        return text.read() }\n";
    common::expect_box_ok_against("anon_capitalized_function_capture", library, source);
}

#[test]
fn captures_generic_constructor_result() {
    run_ok(
        "AnonGenericCtor",
        "interface Text { fun read(): String }\n\
         fun box(): String {\n\
         val values = ArrayList<String>()\n\
         values.add(\"OK\")\n\
         val text = object : Text { override fun read(): String = values[0] }\n\
         return text.read() }\n",
    );
}

#[test]
fn captures_qualified_constructor_result() {
    run_ok(
        "AnonQualifiedCtor",
        "interface Sink { fun add(value: String) }\n\
         fun box(): String {\n\
         val builder = java.lang.StringBuilder()\n\
         val sink = object : Sink { override fun add(value: String) { builder.append(value) } }\n\
         sink.add(\"O\")\n\
         sink.add(\"K\")\n\
         return builder.toString() }\n",
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
         fun mk(seed: Int): T = object : T { val stored: Int = seed * 2; override fun g(): Int = stored }\n\
         fun box(): String { return if (mk(21).g() == 42) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn captures_smart_cast_val() {
    // The capture must record the NARROWED binding (kotlinc accepts): discovery used to keep the
    // outer declared `String?` binding and reject `t.length` inside the object body.
    run_ok(
        "AnonSmartVal",
        "interface L { fun g(): Int }\n\
         fun f(p: String?): Int {\n\
         val t: String? = p\n\
         if (t != null) {\n\
         val o = object : L { override fun g() = t.length }\n\
         return o.g() }\n\
         return -1 }\n\
         fun box(): String {\n\
         if (f(null) != -1) return \"FAIL null\"\n\
         return if (f(\"abc\") == 3) \"OK\" else \"FAIL\" }\n",
    );
}

#[test]
fn captures_inner_shadowed_local() {
    // Plain lexical shadowing: the capture is the INNER `t` (`String`), never the outer `Int`.
    run_ok(
        "AnonInnerShadow",
        "interface L { fun g(): Int }\n\
         fun f(b: Boolean): Int {\n\
         val t: Int = 1\n\
         if (b) {\n\
         val t: String = \"abc\"\n\
         val o = object : L { override fun g() = t.length }\n\
         return o.g() }\n\
         return -1 }\n\
         fun box(): String {\n\
         if (f(false) != -1) return \"FAIL false\"\n\
         return if (f(true) == 3) \"OK\" else \"FAIL\" }\n",
    );
}
