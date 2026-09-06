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
fn mutable_local_capture_uses_one_shared_cell_across_anonymous_class() {
    run_ok(
        "AnonMutableLocal",
        "interface Setter { fun set(value: String) }\n\
         fun box(): String {\n\
         var result = \"fail\"\n\
         val setter = object : Setter {\n\
             override fun set(value: String) { result = value }\n\
         }\n\
         setter.set(\"OK\")\n\
         return result }\n",
    );
}

#[test]
fn nested_anonymous_object_uses_outer_capture_field_identity() {
    run_ok(
        "NestedAnonCaptureField",
        "interface Result { fun value(): String }\n\
         fun box(): String {\n\
             var first = \"O\"\n\
             var second = \"K\"\n\
             val outer = object {\n\
                 fun make(prefix: String) = object : Result {\n\
                     override fun value(): String {\n\
                         first = prefix\n\
                         return first + second\n\
                     }\n\
                 }\n\
             }\n\
             return outer.make(\"O\").value()\n\
         }\n",
    );
}

#[test]
fn generic_override_approximates_anonymous_result_to_declared_supertype() {
    run_ok(
        "GenericAnonymousResult",
        "interface Value<T> { fun get(): T }\n\
         interface Factory { fun <T> make(value: T): Value<T> }\n\
         fun factory(): Factory = object : Factory {\n\
             override fun <T> make(value: T) = object : Value<T> {\n\
                 override fun get(): T = value\n\
             }\n\
         }\n\
         fun box(): String = factory().make(\"OK\").get()\n",
    );
}

#[test]
fn nullable_primitive_capture_uses_reference_shared_cell() {
    run_ok(
        "AnonNullablePrimitiveCapture",
        "interface NullableSetter { fun set(value: Int?) }\n\
         fun box(): String {\n\
         var result: Int? = null\n\
         val setter = object : NullableSetter {\n\
             override fun set(value: Int?) { result = value }\n\
         }\n\
         setter.set(42)\n\
         return if (result == 42) \"OK\" else \"fail\" }\n",
    );
}

#[test]
fn mutable_local_capture_uses_one_shared_cell_across_local_class() {
    run_ok(
        "LocalClassMutableLocal",
        "fun box(): String {\n\
         var result = \"fail\"\n\
         class Setter { fun set(value: String) { result = value } }\n\
         Setter().set(\"OK\")\n\
         return result }\n",
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
fn captures_smart_cast_var() {
    run_ok(
        "AnonSmartVar",
        "interface L { fun g(): Int }\n\
         fun f(p: String?): Int {\n\
         var t: String? = p\n\
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

#[test]
fn pass_two_captures_receiver_function_values_in_anonymous_object() {
    run_ok(
        "AnonReceiverFunctionCapture",
        "interface WriteContext { val prefix: String }\n\
         interface Codec<T> { fun WriteContext.encode(value: T): String }\n\
         fun <T> codec(block: WriteContext.(T) -> String): Codec<T> = object : Codec<T> {\n\
             override fun WriteContext.encode(value: T): String = block(value)\n\
         }\n\
         fun box(): String {\n\
             val codec = codec<String> { prefix + it }\n\
             val context = object : WriteContext { override val prefix: String get() = \"O\" }\n\
             return with(codec) { context.encode(\"K\") }\n\
         }\n",
    );
}

#[test]
fn pass_two_lifts_outer_receiver_through_lambda_into_anonymous_class() {
    run_ok(
        "AnonOuterReceiverThroughLambda",
        "fun execute(block: () -> Unit) { block() }\n\
         class Outer(result: String) {\n\
             var result: String = \"Failed\"\n\
             init {\n\
                 execute {\n\
                     object {\n\
                         init { execute { completed(result) } }\n\
                     }\n\
                 }\n\
             }\n\
             fun completed(value: String) { this.result = value }\n\
         }\n\
         fun box(): String = Outer(\"OK\").result\n",
    );
}

#[test]
fn pass_two_discovers_anonymous_super_outer_receiver_inside_local_class() {
    run_ok(
        "AnonInnerSuperInsideLocal",
        "fun box(): String {\n\
             class Local {\n\
                 open inner class Inner(val value: String) {\n\
                     open fun result(): String = \"Fail\"\n\
                 }\n\
                 val expected = \"OK\"\n\
                 val instance = object : Inner(expected) {\n\
                     override fun result(): String = value\n\
                 }\n\
             }\n\
             return Local().instance.result()\n\
         }\n",
    );
}

#[test]
fn property_initializer_captures_same_named_enclosing_value() {
    run_ok(
        "AnonPropertyInitializerShadow",
        "interface Value { val value: Int; fun read(): Int }\n\
         fun make(value: Int): Value = object : Value {\n\
             override val value: Int = value + 1\n\
             override fun read(): Int = value\n\
         }\n\
         fun box(): String {\n\
             val result = make(41)\n\
             return if (result.value == 42 && result.read() == 42) \"OK\" else \"FAIL\"\n\
         }\n",
    );
}
