//! A Java type variable's bound is a Java type, so it admits null whether it is implicit (`Any!`)
//! or declared (`<N extends Number>`). A member typed by the variable itself (`T get()`) is `T!`,
//! so a receiver argument `X?` specializes it to `(X?..X?)`, which is `X?`. krusty published the
//! occurrence with a non-null bound, read like `T & Any`, narrowed `X?` to `X` and typed the result
//! `X!`: it accepted an unsafe call kotlinc rejects and unboxed a null value-class element. The
//! diagnostics read the fixtures as Java sources (through their signature stubs); the run reads
//! javac's class files, so both Java providers are covered.

use super::common;

const HOLDER: &str = "public class Holder<T> {\n\
    \x20   private T value;\n\
    \x20   public Holder(T value) { this.value = value; }\n\
    \x20   public T get() { return value; }\n\
    \x20   public void set(T value) { this.value = value; }\n\
    }\n";

const NUMBERS: &str = "public class Numbers<N extends Number> {\n\
    \x20   private final N value;\n\
    \x20   public Numbers(N value) { this.value = value; }\n\
    \x20   public N get() { return value; }\n\
    }\n";

const UNSAFE: &str = "fun text(holder: Holder<String?>): Int = holder.get().length\n\
    fun number(numbers: Numbers<Int?>): Int = numbers.get().toInt()\n\
    fun stored(holder: Holder<String?>): Int { val x = holder.get(); return x.length }\n";

const SAFE: &str = "@JvmInline value class Label(val text: String) { fun read(): String = text }\n\
    fun label(holder: Holder<Label?>): String? = holder.get()?.read()\n\
    fun filled(): Holder<Label?> { val holder = Holder<Label?>(null); holder.set(Label(\"a\")); return holder }\n\
    fun text(holder: Holder<String?>): Int = holder.get()?.length ?: -1\n\
    fun number(numbers: Numbers<Int?>): Int = numbers.get() ?: 7\n\
    fun box(): String {\n\
    \x20   if (label(Holder<Label?>(null)) != null || label(filled()) != \"a\") return \"label\"\n\
    \x20   if (text(Holder<String?>(null)) != -1 || text(Holder<String?>(\"ab\")) != 2) return \"text\"\n\
    \x20   return if (number(Numbers<Int?>(null)) == 7 && number(Numbers<Int?>(2)) == 2) \"OK\" else \"number\"\n\
    }\n";

#[test]
fn a_nullable_argument_for_a_java_source_type_variable_rejects_an_unsafe_call() {
    common::assert_errors_match_kotlinc(
        &[
            ("Holder.java", HOLDER),
            ("Numbers.java", NUMBERS),
            ("main.kt", UNSAFE),
        ],
        &[],
    );
}

#[test]
fn a_nullable_argument_for_a_java_type_variable_is_tested_for_null() {
    assert_eq!(
        common::java_interop_box(
            "JavaTypeVariableBound",
            &[("Holder.java", HOLDER), ("Numbers.java", NUMBERS)],
            SAFE,
        ),
        "OK"
    );
}
