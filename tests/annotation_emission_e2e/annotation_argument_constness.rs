//! A non-constant annotation argument is reported on the argument, with kotlinc's diagnostic for
//! its kind: a `val` that could have been `const`, a class literal of a value, or any other
//! non-constant expression. Every ledger is compared with kotlinc's in full.

use crate::common;

/// A class literal whose receiver failed to resolve is not a constant; one whose receiver is a
/// value is not a class literal.
#[test]
fn non_constant_class_literal_arguments_are_reported_on_the_argument() {
    let src = "annotation class C(val k: kotlin.reflect.KClass<*>)
@C(Unknown::class) fun unresolved() {}
fun value(o: Any) { @C(o::class) val y = 1 }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A property whose semantic result is an enum or `KClass` is diagnosed by that type before its
/// declaration flags: the property is neither an enum entry nor a class literal.
#[test]
fn typed_properties_keep_their_annotation_argument_diagnostics() {
    let src = "annotation class EnumArg(val value: Choice)
annotation class ClassArg(val value: kotlin.reflect.KClass<*>)
enum class Choice { A }
val choice = Choice.A
val klass = String::class
@EnumArg(choice) fun enumProperty() {}
@ClassArg(klass) fun classProperty() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Enum/KClass property reads keep their dedicated diagnostics only as direct annotation values.
/// In a template or scalar operator, the complete expression receives the generic diagnostic.
#[test]
fn typed_properties_are_not_scalar_constant_operands() {
    let src = "annotation class A(val value: String)
annotation class EA(val value: E)
annotation class KA(val value: kotlin.reflect.KClass<*>)
enum class E { X }
val moduleEnum = E.X
val moduleKlass = String::class
@EA(moduleEnum) fun directModuleEnum() {}
@KA(moduleKlass) fun directModuleKlass() {}
@A(\"${moduleEnum}\") fun templateModuleEnum() {}
@A(\"x\" + moduleEnum) fun concatModuleEnum() {}
@A(\"${moduleKlass}\") fun templateModuleKlass() {}
@A(\"x\" + moduleKlass) fun concatModuleKlass() {}
fun local() {
    val localEnum = E.X
    val localKlass = String::class
    @EA(localEnum) val directEnum = 0
    @KA(localKlass) val directKlass = 0
    @A(\"${localEnum}\") val templateEnum = 0
    @A(\"x\" + localEnum) val concatEnum = 0
    @A(\"${localKlass}\") val templateKlass = 0
    @A(\"x\" + localKlass) val concatKlass = 0
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// An enum-typed local property keeps the enum-constant diagnostic, while a function parameter of
/// the same semantic type remains an ordinary non-constant value.
#[test]
fn a_local_enum_property_is_not_an_enum_constant() {
    let src = "annotation class A(val value: Choice)
enum class Choice { A }
fun local() {
    val choice = Choice.A
    @A(choice) val use = 0
}
fun parameter(choice: Choice) {
    @A(choice) val use = 0
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Every local-property declaration shape carries the same stable provenance into annotation
/// checking. Its enum-typed read is a property value, not an enum entry.
#[test]
fn local_property_forms_are_not_enum_constants() {
    let src = "annotation class A(val value: Choice)
enum class Choice { A }
data class Box(val choice: Choice)
fun forms() {
    lateinit var late: Choice
    val delegated: Choice by lazy { Choice.A }
    val (destructured) = Box(Choice.A)
    @A(late) val lateUse = 0
    @A(delegated) val delegatedUse = 0
    @A(destructured) val destructuredUse = 0
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A `for` variable is an immutable local property even though lowering may mutate its storage.
/// Its stable flow identity therefore keeps the enum-property diagnostic.
#[test]
fn a_foreach_variable_is_not_an_enum_constant() {
    let src = "annotation class A(val value: Choice)
enum class Choice { A }
fun use(values: Array<Choice>) {
    for (choice in values) {
        @A(choice) val use = 0
    }
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Unlike a module property, a local property or parameter of `KClass` type is an ordinary lexical
/// value. Kotlinc diagnoses both generically rather than treating either read as a class literal.
#[test]
fn a_local_kclass_value_is_not_a_class_literal() {
    let src = "annotation class K(val value: kotlin.reflect.KClass<*>)
fun local() {
    val klass: kotlin.reflect.KClass<*> = String::class
    @K(klass) val use = 0
}
fun parameter(klass: kotlin.reflect.KClass<*>) {
    @K(klass) val use = 0
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// An indexed read is a non-constant expression as a whole. Its generic diagnostic covers the
/// complete index expression instead of falling through to the annotation-level folding error.
#[test]
fn an_indexed_argument_is_not_a_constant() {
    let src = "annotation class A(val value: Int)
fun indexed(values: IntArray) {
    @A(values[0]) val use = 0
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A type test is a non-constant expression as a whole. Its generic diagnostic covers the complete
/// `is` expression instead of falling through to the annotation-level folding error.
#[test]
fn a_type_test_argument_is_not_a_constant() {
    let src = "annotation class A(val value: Boolean)
fun typeTest(value: Any) {
    @A(value is String) val use = 0
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Elvis is a typed, non-constant expression of its own. Its diagnostic covers the complete Elvis
/// expression rather than falling through to the annotation-level unsupported-constant error.
#[test]
fn an_elvis_argument_is_not_a_constant() {
    let src = "annotation class A(val value: String)
var nullable: String? = null
@A(nullable ?: \"fallback\") fun use() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A not-null assertion is a non-constant expression of its own. The complete assertion receives
/// the diagnostic rather than falling through to the annotation-level unsupported-constant error.
#[test]
fn a_not_null_argument_is_not_a_constant() {
    let src = "annotation class A(val value: String)
var text: String? = null
@A(text!!) fun use() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Unary plus is part of a numeric literal initializer, just like unary minus. A non-`const` `val`
/// initialized by it therefore receives kotlinc's more specific constant-expression diagnostic.
#[test]
fn unary_plus_is_a_literal_initializer() {
    let src = "annotation class A(val value: Int)
val number = +1
@A(number) fun use() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A module `val` with a default getter and a literal initializer could have been `const`, however
/// it is reached: by name, through `this`, an instance, an object or a companion.
#[test]
fn a_val_with_a_literal_initializer_could_have_been_const() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
annotation class L(val l: Long)
annotation class B(val b: Boolean)
val text = \"a\"
val negative = -1
val parenthesized = (2)
val widened: Long = 1
val flag = true
val raw = \"\"\"raw\"\"\"
class C {
    val member = \"a\"
    private val hidden = \"x\"
    fun f() { @A(member) val y = 1; @A(this.member) val z = 1 }
    @A(hidden) fun g() {}
    companion object { val shared = \"a\" }
}
object O { val single = \"a\" }
fun instance(c: C) { @A(c.member) val y = 1 }
@A(text) fun a1() {}
@I(negative) fun a2() {}
@I(parenthesized) fun a3() {}
@L(widened) fun a4() {}
@B(flag) fun a5() {}
@A(raw) fun a6() {}
@A(O.single) fun a7() {}
@A(C.shared) fun a8() {}
@A((text)) fun a9() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Any other read is not a compile-time constant: a `var`, a custom or delegated getter, a
/// non-literal initializer, a local, a parameter, a constructor property, a dependency's property,
/// a branch, and a call of a module function.
#[test]
fn other_non_constant_reads_are_not_constants() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
fun s() = \"a\"
const val k = \"k\"
var mutable = \"a\"
val getter get() = \"a\"
val delegated: String by lazy { \"a\" }
val computed = s()
val concatenated = \"a\" + \"b\"
val aliased = k
lateinit var late: String
val String.extension get() = \"a\"
class C(val parameter: String) { companion object { var counter = \"a\" } }
@A(mutable) fun a1() {}
@A(getter) fun a2() {}
@A(delegated) fun a3() {}
@A(computed) fun a4() {}
@A(concatenated) fun a5() {}
@A(aliased) fun a6() {}
@A(late) fun a7() {}
@A(\"a\".extension) fun a8() {}
@A(C.counter) fun a9() {}
@I(KotlinVersion.CURRENT.major) fun a10() {}
@A(java.io.File.separator) fun a11() {}
@A(if (true) \"a\" else \"b\") fun a12() {}
@A(s()) fun a13() {}
fun locals(q: String, c: C) {
    val l = \"a\"
    @A(q) val x = 1
    @A(l) val y = 1
    @A(c.parameter) val z = 1
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// An operator is as constant as its first non-constant operand, and the argument carries that
/// operand's diagnostic.
#[test]
fn an_operator_reports_its_first_non_constant_operand() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
fun s() = \"a\"
val text = \"a\"
var mutable = \"a\"
@A(text + \"b\") fun a1() {}
@A(\"x\" + text) fun a2() {}
@A(text + mutable) fun a3() {}
@A(mutable + text) fun a4() {}
@A(\"a\" + s()) fun a5() {}
@I(-KotlinVersion.CURRENT.major) fun a6() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// An operator expression only inherits an operand's constant-expression kind when overload
/// selection chose a compiler intrinsic. A source operator call is an ordinary non-constant call,
/// rather than the more specific kind one of its operands would earn on its own.
#[test]
fn a_source_operator_is_not_a_constant_intrinsic() {
    let src = "annotation class A(val s: String)
enum class E {
    X;
    operator fun unaryMinus(): String = \"x\"
}
operator fun Int.plus(marker: E): String = marker.name
val number = 1
@A(number + E.X) fun binary() {}
@A(-E.X) fun unary() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A template reports its first non-constant interpolation's kind once, on the complete template.
#[test]
fn a_template_reports_its_first_non_constant_part() {
    let src = "annotation class A(val s: String)
const val constant = \"c\"
val text = \"a\"
var mutable = \"b\"
enum class E { X }
annotation class K(val e: E)
@A(\"$constant$text$mutable\") fun ordered() {}
@A(\"prefix ${text} suffix\") fun braced() {}
@A(\"${E.X}\") fun enumEntry() {}
@K(E.X) fun directEnumEntry() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A class literal is legal as a direct `KClass` annotation value, but it is not a scalar constant
/// that a template or operator can consume. Embedded uses report on their complete expression.
#[test]
fn a_class_literal_is_not_a_constant_operand() {
    let src = "annotation class A(val value: String)
annotation class K(val value: kotlin.reflect.KClass<*>)
@A(\"${String::class}\") fun template() {}
@A(\"prefix \" + String::class) fun operator() {}
@K(String::class) fun direct() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A typed expression wrapper reports its operand's constant-expression kind on the complete
/// expression. This path has no property lowering of its own to classify.
#[test]
fn a_cast_reports_its_non_constant_operands_kind() {
    let src = "annotation class A(val s: String)
val immutable: Any = \"x\"
var mutable: Any = \"x\"
fun text(): Any = \"x\"
@A(immutable as String) fun immutableUse() {}
@A(mutable as String) fun mutableUse() {}
@A(text() as String) fun callUse() {}
fun local(value: Any) { @A(value as String) val use = 1 }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Every source-declared callable shape is an ordinary non-constant call. The diagnostic does not
/// depend on whether selection represented it as a local, top-level extension, member, companion,
/// or member extension.
#[test]
fn source_callable_shapes_are_not_annotation_constants() {
    let src = "annotation class Inner(val s: String)
annotation class Outer(val inner: Inner)
fun String.topExtension(): String = this
class C {
    fun member(): String = \"m\"
    fun String.extension(): String = this
    companion object { fun companion(): String = \"c\" }

    fun annotations() {
        fun local(): String = \"l\"
        @Outer(Inner(member())) val memberUse = 1
        @Outer(Inner(\"e\".extension())) val extensionUse = 1
        @Outer(Inner(local())) val localUse = 1
    }
}
@Outer(Inner(C.companion())) fun companionUse() {}
@Outer(Inner(\"t\".topExtension())) fun topExtensionUse() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A selected constructor is an ordinary non-constant call even though its semantic target is
/// stored separately from function calls. The complete construction receives the diagnostic.
#[test]
fn a_constructor_call_is_not_an_annotation_constant() {
    let src = "annotation class A(val value: String)
@A(String(charArrayOf('x'))) fun use() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A dependency call has the same non-constant-call semantics as every source call; declaration
/// provenance affects linkage, not its annotation diagnostic.
#[test]
fn a_dependency_call_is_not_an_annotation_constant() {
    let library = common::compile_lib(
        "annotation-argument-constness-dependency",
        "package lib\nfun text(): String = \"x\"\n",
    )
    .expect("compile annotation-argument dependency");
    let src = "import lib.text
annotation class A(val s: String)
@A(text()) fun use() {}
";
    let result =
        common::compiler_diagnostics(&[("main.kt", src)], &[library, common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "dependency annotation argument call");
}

/// An array literal reports its aggregate non-const diagnostic before each non-constant element.
/// A value KClass literal keeps only its dedicated element diagnostic, as kotlinc does.
#[test]
fn an_array_literal_retains_its_aggregate_diagnostic() {
    let src = "annotation class A(val values: Array<String>)
annotation class K(val values: Array<kotlin.reflect.KClass<*>>)
val text = \"a\"
var mutable = \"b\"
@A([text]) fun one() {}
@A([mutable]) fun two() {}
fun values(o: Any) { @K([o::class]) val x = 1 }
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

#[test]
fn a_nested_annotation_takes_its_first_non_constant_arguments_kind() {
    let src = "annotation class Inner(val s: String, val t: String = \"\")
annotation class Outer(val inner: Inner)
annotation class Many(val values: Array<String>)
annotation class ManyOuter(val inner: Many)
annotation class Varargs(vararg val values: String)
annotation class VarargOuter(val inner: Varargs)
enum class E { X }
annotation class EnumInner(val e: E, val s: String)
annotation class EnumOuter(val inner: EnumInner)
val text = \"a\"
var mutable = \"a\"
@Outer(Inner(text)) fun n1() {}
@Outer(Inner(\"x\", mutable)) fun n2() {}
@Outer(Inner(t = text, s = mutable)) fun n3() {}
@ManyOuter(Many([text])) fun n4() {}
@ManyOuter(Many([mutable])) fun n5() {}
@VarargOuter(Varargs(text)) fun n6() {}
@VarargOuter(Varargs(mutable)) fun n7() {}
@EnumOuter(EnumInner(E.X, text)) fun n8() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// kotlinc evaluates only an `@IntrinsicConstEvaluation` callee, so a call of any other library
/// function is not a constant either.
#[test]
fn a_library_call_is_not_a_constant() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
@I(maxOf(1, 2)) fun l1() {}
@A(listOf(\"a\").first()) fun l2() {}
@A(\"a\".uppercase()) fun l3() {}
@A(\"a\".repeat(2)) fun l4() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// Each array element is checked on its own, and an array with a non-constant element is also
/// reported, ahead of its elements, as a value only `const val`s could build: an array literal,
/// an `arrayOf` call and a spread vararg operand alike. Elements that are not class literals do
/// not make their array non-constant, and inside a nested annotation an array takes its first
/// non-constant element's kind.
#[test]
fn an_array_with_a_non_constant_element_is_reported_with_its_elements() {
    let src = "import kotlin.reflect.KClass
annotation class B(val s: Array<String>)
annotation class K(val k: Array<KClass<*>>)
annotation class I(val i: IntArray)
annotation class V(vararg val s: String)
annotation class N(val n: Array<B>)
annotation class M(val b: B)
val text = \"t\"
val kc = String::class
fun call() = \"c\"
@B([text]) fun a1() {}
@B([call(), text]) fun a2() {}
@B([\"a\", unresolved]) fun a3() {}
@K([String::class, text::class]) fun a4() {}
@I([1, if (true) 2 else 3]) fun a5() {}
@B(arrayOf(call())) fun a6() {}
@B(s = [call()]) fun a7() {}
@K([kc, text::class]) fun a8() {}
@K([String::class, if (true) kc else kc]) fun a9() {}
@V(call()) fun a10() {}
@V(\"a\", text) fun a11() {}
@V(*arrayOf(call()), text) fun a12() {}
@B(arrayOf(*arrayOf(text))) fun a13() {}
@N([B([call()])]) fun a14() {}
@M(B([call()])) fun a15() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A property read or call producing a `KClass` is not a class literal; a local or parameter is
/// simply not a constant.
#[test]
fn a_kclass_value_is_not_a_class_literal() {
    let src = "import kotlin.reflect.KClass
annotation class K1(val k: KClass<*>)
annotation class K(val k: Array<KClass<*>>)
annotation class NK(val k: K)
val kc = String::class
fun kcall(): KClass<*> = Int::class
@K1(kc) fun c1() {}
@K1(kcall()) fun c2() {}
@K([kcall()]) fun c3() {}
@K(arrayOf(kc)) fun c4() {}
@NK(K([kc])) fun c5() {}
fun f(p: KClass<*>) {
    val l = String::class
    @K1(l) val x = 1
    @K1(p) val y = 2
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A safe call, range test, `try`, scope-function call or subjectless `when` is never a constant,
/// whatever its operands; a parenthesized expression is its content, and a cast of a constant to
/// its own type is still that constant.
#[test]
fn other_expression_forms_are_not_constants() {
    let src = "annotation class A(val s: String)
annotation class I(val i: Int)
annotation class Bo(val b: Boolean)
val text = \"t\"
var mutable = \"m\"
val nullable: String? = \"n\"
const val C = \"c\"
@A(C!!) fun e1() {}
@I(nullable?.length ?: 0) fun e2() {}
@Bo(!(text is String)) fun e3() {}
@A(try { \"a\" } catch (e: Exception) { \"b\" }) fun e4() {}
@Bo(1 in 0..2) fun e5() {}
@A(run { \"a\" }) fun e6() {}
@A(when { else -> \"a\" }) fun e7() {}
@A((text)) fun e8() {}
@A((mutable)) fun e9() {}
@A(C as String) fun e10() {}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}
