use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn build_list_reads_enclosing_field() {
    const SRC: &str = "class C(val prefix: String) {\n\
        fun items(): List<String> = buildList { add(prefix); add(prefix + \"!\") }\n\
    }\n\
    fun box(): String = C(\"a\").items().joinToString(\",\")\n";
    assert_eq!(run(SRC), Some("a,a!".to_string()));
}

#[test]
fn build_list_loops_over_enclosing_field() {
    const SRC: &str = "class C(val tag: String, val xs: List<Int>) {\n\
        fun rows(): List<String> = buildList { for (x in xs) { add(\"$tag=$x\") } }\n\
    }\n\
    fun box(): String = C(\"n\", listOf(1, 2)).rows().joinToString(\",\")\n";
    assert_eq!(run(SRC), Some("n=1,n=2".to_string()));
}

#[test]
fn build_list_calls_enclosing_method() {
    const SRC: &str = "class C(val base: Int) {\n\
        private fun scaled(x: Int): Int = x * base\n\
        fun items(): List<Int> = buildList { add(scaled(1)); add(scaled(2)) }\n\
    }\n\
    fun box(): String = C(10).items().joinToString(\",\")\n";
    assert_eq!(run(SRC), Some("10,20".to_string()));
}

#[test]
fn enclosing_overload_uses_checker_selection() {
    const SRC: &str = "class C {\n\
        fun scaled(x: String): String = x + \"!\"\n\
        fun scaled(x: Int): Int = x * 2\n\
        fun items(): List<Int> = buildList { add(scaled(3)) }\n\
    }\n\
    fun box(): String = C().items().single().toString()\n";
    assert_eq!(run(SRC), Some("6".to_string()));
}

#[test]
fn enclosing_method_preserves_named_default_mapping() {
    const SRC: &str = "class C {\n\
        fun render(prefix: String = \"v\", value: Int): String = prefix + value\n\
        fun items(): List<String> = buildList { add(render(value = 3)) }\n\
    }\n\
    fun box(): String = C().items().single()\n";
    assert_eq!(run(SRC), Some("v3".to_string()));
}

#[test]
fn build_string_reads_enclosing_field() {
    const SRC: &str = "class C(val name: String) {\n\
        fun render(): String = buildString { append(\"[\"); append(name); append(\"]\") }\n\
    }\n\
    fun box(): String = C(\"x\").render()\n";
    assert_eq!(run(SRC), Some("[x]".to_string()));
}

#[test]
fn receiver_wins_over_enclosing_on_name_clash() {
    const SRC: &str = "class C(val size: Int) {\n\
        fun f(): Int = buildList { add(\"a\"); add(\"b\"); add(size.toString()) }.size\n\
    }\n\
    fun box(): String = C(99).f().toString()\n";
    assert_eq!(run(SRC), Some("3".to_string()));
}

#[test]
fn build_list_without_enclosing_use() {
    const SRC: &str = "class C {\n\
        fun items(): List<String> = buildList { add(\"a\"); add(\"b\") }\n\
    }\n\
    fun box(): String = C().items().joinToString(\",\")\n";
    assert_eq!(run(SRC), Some("a,b".to_string()));
}

#[test]
fn receiver_lambda_without_outer_use_in_value_class() {
    const SRC: &str = "@JvmInline value class V(val n: Int) {\n\
        fun items(): List<Int> = buildList { add(1); add(2) }\n\
    }\n\
    fun box(): String = V(9).items().joinToString(\",\")\n";
    assert_eq!(run(SRC), Some("1,2".to_string()));
}

#[test]
fn nested_generic_receiver_lambda_keeps_inferred_return() {
    const SRC: &str = "class Outer<A> {\n\
            inner class Scope<B>\n\
            fun <B> nested(block: Scope<B>.() -> String) = Scope<B>().block()\n\
        }\n\
        fun <A> outer(block: Outer<A>.() -> String) = Outer<A>().block()\n\
        fun box() = outer<Int> { nested<Boolean> { \"OK\" } }\n";
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn inherited_generic_method_keeps_inferred_return() {
    const SRC: &str = "abstract class Base<T> {\n\
            fun convert(value: T?, transform: (T) -> Any?) = value?.let { transform(it) }\n\
        }\n\
        @JvmInline value class Wrapped<T : Any>(val value: T?)\n\
        class Derived : Base<Wrapped<String>>()\n\
        fun box() = Derived().convert(Wrapped(\"OK\")) { it.value } as String\n";
    assert_eq!(run(SRC), Some("OK".to_string()));
}
