use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn named_arguments_on_a_safe_member_call() {
    const SRC: &str = "class W(val tag: String) {\n\
    \x20   fun mix(a: String, b: String): String = tag + a + b\n\
    }\n\
    fun box(): String {\n\
    \x20   val w: W? = W(\"t\")\n\
    \x20   return w?.mix(b = \"Y\", a = \"X\") ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("safe member call"), "tXY");
}

#[test]
fn qualified_named_call_evaluates_receiver_before_arguments() {
    const SRC: &str = "var trace = \"\"\n\
    class W {\n\
    \x20   fun mix(a: String, b: String): String = a + b\n\
    }\n\
    fun receiver(): W { trace += \"R\"; return W() }\n\
    fun mark(value: String): String { trace += value; return value }\n\
    fun box(): String {\n\
    \x20   val result = receiver().mix(b = mark(\"B\"), a = mark(\"A\"))\n\
    \x20   return result + \"/\" + trace\n\
    }\n";
    assert_eq!(run(SRC).expect("qualified named call"), "AB/RBA");
}

#[test]
fn classpath_named_call_evaluates_receiver_before_arguments() {
    const LIB: &str = "package lib\n\
    class W { fun mix(a: String, b: String): String = a + b }\n";
    const MAIN: &str = "import lib.W\n\
    var trace = \"\"\n\
    fun receiver(): W { trace += \"R\"; return W() }\n\
    fun mark(value: String): String { trace += value; return value }\n\
    fun box(): String {\n\
    \x20   val result = receiver().mix(b = mark(\"B\"), a = mark(\"A\"))\n\
    \x20   return if (result == \"AB\" && trace == \"RBA\") \"OK\" else result + \"/\" + trace\n\
    }\n";
    common::expect_box_ok_against("named_call_receiver_order", LIB, MAIN);
}

#[test]
fn source_extension_named_call_keeps_source_evaluation_order() {
    const SRC: &str = "var trace = \"\"\n\
    fun receiver(): String { trace += \"R\"; return \"x\" }\n\
    fun mark(value: String): String { trace += value; return value }\n\
    fun String.mix(a: String, b: String = \"B\", c: String): String = this + a + b + c\n\
    fun box(): String {\n\
    \x20   val result = receiver().mix(c = mark(\"C\"), a = mark(\"A\"))\n\
    \x20   return result + \"/\" + trace\n\
    }\n";
    assert_eq!(run(SRC).expect("source extension named call"), "xABC/RCA");
}

#[test]
fn module_extension_named_call_keeps_source_evaluation_order() {
    const LIBRARY: &str =
        "fun String.mix(a: String, b: String, c: String): String = this + a + b + c\n";
    const MAIN: &str = "var trace = \"\"\n\
    fun receiver(): String { trace += \"R\"; return \"x\" }\n\
    fun mark(value: String): String { trace += value; return value }\n\
    fun box(): String {\n\
    \x20   val result = receiver().mix(c = mark(\"C\"), a = mark(\"A\"), b = mark(\"B\"))\n\
    \x20   return if (result == \"xABC\" && trace == \"RCAB\") \"OK\" else result + \"/\" + trace\n\
    }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Library.kt", LIBRARY), ("Main.kt", MAIN)],
        "module_extension_named_order",
    );
}

#[test]
fn classpath_extension_named_call_keeps_source_evaluation_order() {
    const LIBRARY: &str = "package lib\n\
    fun String.mix(a: String, b: String, c: String): String = this + a + b + c\n";
    const MAIN: &str = "import lib.mix\n\
    var trace = \"\"\n\
    fun receiver(): String { trace += \"R\"; return \"x\" }\n\
    fun mark(value: String): String { trace += value; return value }\n\
    fun box(): String {\n\
    \x20   val result = receiver().mix(c = mark(\"C\"), a = mark(\"A\"), b = mark(\"B\"))\n\
    \x20   return if (result == \"xABC\" && trace == \"RCAB\") \"OK\" else result + \"/\" + trace\n\
    }\n";
    common::expect_box_ok_against("classpath_extension_named_order", LIBRARY, MAIN);
}

#[test]
fn named_value_class_extension_keeps_the_logical_receiver_representation() {
    const SRC: &str = "fun inspect(value: Result<Int>): String = value.fold(\n\
        \x20 onFailure = { \"failure\" },\n\
        \x20 onSuccess = { it.toString() },\n\
        )\n\
        fun box(): String = if (inspect(Result.success(7)) == \"7\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("named value-class extension"), "OK");
}

#[test]
fn spread_argument_on_a_safe_member_call() {
    const SRC: &str = "class W {\n\
    \x20   fun join(vararg parts: String): String = parts.joinToString(\"\")\n\
    }\n\
    fun box(): String {\n\
    \x20   val w: W? = W()\n\
    \x20   val xs = arrayOf(\"a\", \"b\")\n\
    \x20   return w?.join(*xs) ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("safe spread call"), "ab");
}

#[test]
fn named_arguments_on_a_safe_extension_call() {
    const SRC: &str = "fun String.mix(a: Int, b: String): String = this + a.toString() + b\n\
    fun box(): String {\n\
    \x20   val s: String? = \"t\"\n\
    \x20   return s?.mix(b = \"Y\", a = 2) ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("safe extension call"), "t2Y");
}

#[test]
fn named_argument_with_a_default_on_a_safe_extension_call() {
    const SRC: &str =
        "fun String.mix(a: String = \"A\", b: String = \"B\"): String = this + a + b\n\
    fun box(): String {\n\
    \x20   val s: String? = \"t\"\n\
    \x20   return s?.mix(b = \"Y\") ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("safe extension default call"), "tAY");
}

#[test]
fn named_safe_extension_uses_closest_receiver() {
    const SRC: &str = "fun Any.pick(value: Int): String = \"any\"\n\
    fun String.pick(value: Int): String = \"string\"\n\
    fun box(): String {\n\
    \x20   val value: String? = \"x\"\n\
    \x20   return value?.pick(value = 1) ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("ranked safe extension"), "string");
}

#[test]
fn named_safe_extension_uses_the_most_specific_value_parameter() {
    const SRC: &str = "fun String.pick(value: Any): String = \"any\"\n\
    fun String.pick(value: CharSequence): String = \"chars\"\n\
    fun box(): String {\n\
    \x20   val receiver: String? = \"x\"\n\
    \x20   return receiver?.pick(value = \"value\") ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("most specific named extension"), "chars");
}

#[test]
fn inapplicable_member_does_not_block_reordered_named_extension() {
    const SRC: &str = "class W {\n\
    \x20   fun pick(a: Int, b: String): String = \"member\"\n\
    }\n\
    fun W.pick(a: String, b: Int): String = \"extension\" + a + b\n\
    fun box(): String {\n\
    \x20   val receiver: W? = W()\n\
    \x20   return receiver?.pick(b = 1, a = \"A\") ?: \"none\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("named member extension fallback"),
        "extensionA1"
    );
}

#[test]
fn inapplicable_member_overloads_do_not_block_safe_extension() {
    const MODEL: &str = "package model\n\
    class W {\n\
    \x20   fun pick(a: Int, b: String): String = \"first\"\n\
    \x20   fun pick(a: Boolean, b: Long): String = \"second\"\n\
    }\n";
    const EXTENSION: &str = "package extensions\n\
    import model.W\n\
    fun W.pick(a: String, b: Int): String = \"extension\" + a + b\n";
    const MAIN: &str = "import extensions.pick\n\
    import model.W\n\
    fun box(): String {\n\
    \x20   val receiver: W? = W()\n\
    \x20   val result = receiver?.pick(b = 1, a = \"A\") ?: \"none\"\n\
    \x20   return if (result == \"extensionA1\") \"OK\" else result\n\
    }\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Model.kt", MODEL),
            ("Extension.kt", EXTENSION),
            ("Main.kt", MAIN),
        ],
        "safe_imported_extension_after_member_overloads",
    );
}

#[test]
fn named_safe_extension_rank_beats_declaration_origin() {
    const LIB: &str = "package lib\n\
    fun String.pick(value: Int): String = \"library\" + value\n";
    const MAIN: &str = "import lib.pick\n\
    fun Any.pick(value: Int): String = \"source\" + value\n\
    fun box(): String {\n\
    \x20   val value: String? = \"x\"\n\
    \x20   val result = value?.pick(value = 1) ?: \"none\"\n\
    \x20   return if (result == \"library1\") \"OK\" else result\n\
    }\n";
    common::expect_box_ok_against("safe_extension_receiver_rank", LIB, MAIN);
}

#[test]
fn invalid_named_safe_extension_does_not_resolve_positionally() {
    const SRC: &str = "fun String.pick(value: Int): String = value.toString()\n\
    fun box(): String {\n\
    \x20   val value: String? = \"x\"\n\
    \x20   return value?.pick(missing = 1) ?: \"none\"\n\
    }\n";
    let stdlib = common::stdlib_jar();
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], Some(common::jdk_modules().as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("parameter") || diagnostic.contains("candidate")),
        "expected invalid named extension diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn equally_ranked_named_safe_extensions_are_ambiguous() {
    const SRC: &str = "interface A\n\
    interface B\n\
    class C : A, B\n\
    fun A.pick(value: Int): String = \"a\"\n\
    fun B.pick(value: Int): String = \"b\"\n\
    fun box(): String {\n\
    \x20   val value: C? = C()\n\
    \x20   return value?.pick(value = 1) ?: \"none\"\n\
    }\n";
    let stdlib = common::stdlib_jar();
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], Some(common::jdk_modules().as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambigu")),
        "expected extension ambiguity diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn expanded_positional_source_extension_vararg_is_packed() {
    const SRC: &str = "fun String.join(prefix: String, vararg values: String): String =\n\
    \x20   this + prefix + values.joinToString(\"\")\n\
    fun box(): String {\n\
    \x20   val value: String? = \"t\"\n\
    \x20   return value?.join(\"P\", \"X\", \"Y\") ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("source extension vararg"), "tPXY");
}

#[test]
fn named_arguments_on_a_safe_library_extension_call() {
    const SRC: &str = "fun box(): String {\n\
    \x20   val s: String? = \"aba\"\n\
    \x20   return s?.replace(newValue = \"x\", oldValue = \"a\") ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("safe library extension call"), "xbx");
}

#[test]
fn named_arguments_on_a_safe_classpath_member_call() {
    const LIB: &str = "package lib\n\
    class W(val tag: String) {\n\
    \x20   fun mix(a: String, b: String): String = tag + a + b\n\
    \x20   fun join(vararg values: String): String = values.joinToString(\"\")\n\
    \x20   fun defaults(a: String = \"A\", b: String = \"B\"): String = a + b\n\
    }\n";
    const MAIN: &str = "import lib.W\n\
    var trace = \"\"\n\
    fun mark(value: String): String {\n\
    \x20   trace += value\n\
    \x20   return value\n\
    }\n\
    fun box(): String {\n\
    \x20   val w: W? = W(\"t\")\n\
    \x20   val result = w?.mix(b = mark(\"Y\"), a = mark(\"X\")) ?: \"none\"\n\
    \x20   val joined = w?.join(\"a\", \"b\") ?: \"none\"\n\
    \x20   val defaults = w?.defaults(b = \"Y\") ?: \"none\"\n\
    \x20   return if (result == \"tXY\" && trace == \"YX\" && joined == \"ab\" && defaults == \"AY\") \"OK\" else result + \"/\" + trace + \"/\" + joined + \"/\" + defaults\n\
    }\n";
    common::expect_box_ok_against_ref("safe_call_named_classpath", LIB, MAIN);
}

#[test]
fn named_argument_on_a_safe_scope_call() {
    const SRC: &str = "fun box(): String {\n\
    \x20   val s: String? = \"x\"\n\
    \x20   return s?.let(block = { it + \"!\" }) ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("safe scope call"), "x!");
}

#[test]
fn named_argument_with_a_default_on_a_safe_member_call() {
    const SRC: &str = "data class P(val path: String, val n: Int)\n\
    fun box(): String {\n\
    \x20   val p: P? = P(\"old\", 2)\n\
    \x20   val result = p?.copy(path = \"new\") ?: return \"none\"\n\
    \x20   return result.path + \"/\" + result.n\n\
    }\n";
    assert_eq!(run(SRC).expect("safe default call"), "new/2");
}

#[test]
fn inline_named_arguments_keep_source_evaluation_order() {
    const SRC: &str = "class W(val tag: String) {\n\
    \x20   inline fun mix(a: String, b: String): String = tag + a + b\n\
    }\n\
    var trace = \"\"\n\
    fun mark(value: String): String {\n\
    \x20   trace += value\n\
    \x20   return value\n\
    }\n\
    fun box(): String {\n\
    \x20   val w: W? = W(\"t\")\n\
    \x20   val result = w?.mix(b = mark(\"B\"), a = mark(\"A\")) ?: \"none\"\n\
    \x20   return result + \"/\" + trace\n\
    }\n";
    assert_eq!(run(SRC).expect("inline safe call"), "tAB/BA");
}

#[test]
fn inline_named_vararg_uses_parameter_slots() {
    const SRC: &str = "class W {\n\
    \x20   inline fun join(prefix: String, vararg values: String): String =\n\
    \x20       prefix + values.joinToString(\"\")\n\
    }\n\
    var trace = \"\"\n\
    fun mark(value: String): String {\n\
    \x20   trace += value\n\
    \x20   return value\n\
    }\n\
    fun box(): String {\n\
    \x20   val w: W? = W()\n\
    \x20   val result = w?.join(\n\
    \x20       values = *arrayOf(mark(\"X\"), mark(\"Y\")),\n\
    \x20       prefix = mark(\"P\"),\n\
    \x20   ) ?: \"none\"\n\
    \x20   return result + \"/\" + trace\n\
    }\n";
    assert_eq!(run(SRC).expect("inline named vararg"), "PXY/XYP");
}

#[test]
fn inline_named_call_synthesizes_omitted_vararg() {
    const SRC: &str = "class W {\n\
    \x20   inline fun join(prefix: String, vararg values: String): String =\n\
    \x20       prefix + values.size\n\
    }\n\
    fun box(): String {\n\
    \x20   val value: W? = W()\n\
    \x20   return value?.join(prefix = \"P\") ?: \"none\"\n\
    }\n";
    assert_eq!(run(SRC).expect("omitted inline vararg"), "P0");
}

#[test]
fn null_safe_call_does_not_evaluate_arguments() {
    const SRC: &str = "class W {\n\
    \x20   fun mix(a: String, b: String): String = a + b\n\
    }\n\
    var trace = \"\"\n\
    fun mark(value: String): String {\n\
    \x20   trace += value\n\
    \x20   return value\n\
    }\n\
    fun box(): String {\n\
    \x20   val w: W? = null\n\
    \x20   val result = w?.mix(b = mark(\"B\"), a = mark(\"A\")) ?: \"none\"\n\
    \x20   return result + \"/\" + trace\n\
    }\n";
    assert_eq!(run(SRC).expect("null safe call"), "none/");
}
