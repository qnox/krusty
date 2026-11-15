//! Under `+IntrinsicConstEvaluation`, `KCallable.name` of a reference literal is a compile-time
//! constant: the selected declaration's name, `<init>` for a constructor, and the interface's name
//! for a fun-interface constructor. A reference bound to an implicit `this` or to a value that is
//! not itself a constant is not.

use super::common;

#[test]
fn a_reference_literal_name_is_an_annotation_and_const_val_constant() {
    const SRC: &str = r#"
// LANGUAGE: +IntrinsicConstEvaluation
@Retention(AnnotationRetention.RUNTIME)
annotation class Names(vararg val values: String)
class A(val p: Int) {
    fun foo() {}
    fun A() {}
    suspend fun bar() {}
}
object O { fun f() {} }
fun interface Action { fun run() }
val top = 1
fun Int.ext() {}
const val PROPERTY = A::p.name
const val MEMBER = A::foo.name
const val SAME_AS_CLASS = A::A.name
const val SUSPEND = A::bar.name
const val CONSTRUCTOR = ::A.name
const val FUN_INTERFACE = ::Action.name
const val TOP = ::top.name
const val CONSTANT_RECEIVER = 42::ext.name
const val SINGLETON = O::f.name
const val CONCATENATED = A::p.name + "!"
@Names(A::p.name, ::Action.name, "x"::length.name) class Annotated
fun box(): String {
    val constants = listOf(
        PROPERTY, MEMBER, SAME_AS_CLASS, SUSPEND, CONSTRUCTOR, FUN_INTERFACE, TOP,
        CONSTANT_RECEIVER, SINGLETON, CONCATENATED,
    ).joinToString()
    if (constants != "p, foo, A, bar, <init>, Action, top, ext, f, p!") return constants
    val annotated = Annotated::class.java.getAnnotation(Names::class.java).values.joinToString()
    return if (annotated == "p, Action, length") "OK" else annotated
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "CallableReferenceNameConstant");
}

/// A reference bound to an implicit `this` or to a non-constant value has no constant name, a local
/// function's reference does, and without `+IntrinsicConstEvaluation` no reference name is a
/// constant.
#[test]
fn a_reference_name_is_constant_only_without_a_non_constant_receiver() {
    let body = "annotation class Ann(val s: String)
class A(val p: Int) {
    fun g() {}
    @Ann(::g.name) fun implicitThis() {}
}
fun make(): A = A(1)
@Ann(make()::p.name) fun valueReceiver() {}
";
    let with = format!(
        "// LANGUAGE: +IntrinsicConstEvaluation\n{body}@Ann(A::p.name) fun constant() {{}}\nfun locals() {{\n    fun local() {{}}\n    @Ann(::local.name) val name = 1\n}}\n"
    );
    common::assert_errors_match_kotlinc(
        &[("main.kt", &with)],
        &common::language_directives::kotlinc_args(&with),
    );
    let without = format!("{body}@Ann(A::p.name) fun notConstant() {{}}\n");
    common::assert_errors_match_kotlinc(&[("main.kt", &without)], &[]);
}
