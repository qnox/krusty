//! Value classes through the native code generator.
//!
//! What a value class IS comes from the checked declarations, and how this target carries one is
//! its representation policy (`native::value_classes`): a non-null `V` is its value, a `V?` is its
//! value where the value's own `null` is free to mean the outer one, and a BOX of `V`'s own type
//! everywhere a position does not know it holds a `V`. These programs cross every one of those
//! boundaries, because a crossing is where a wrong representation shows: a box answering as the
//! value inside would render `1` for `V(n=1)` and say `false` to `is V`.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{
    expect_box_ok_files_with_stdlib, expect_box_ok_with_stdlib, expect_native_box,
    expect_native_sources, kotlin_native_box, kotlinc_box_result,
};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// `equals`, `hashCode` and `toString` are the value's, and a member sees the value as `this`.
#[test]
fn a_value_class_answers_by_the_value_it_wraps() {
    let source = "@JvmInline value class Count(val n: Int) {\n\
         \x20   fun doubled(): Int = n * 2\n\
         }\n\
         @JvmInline value class Label(val text: String)\n\
         fun box(): String {\n\
         \x20   if (Count(1) != Count(1)) return \"fail 1\"\n\
         \x20   if (Count(1) == Count(2)) return \"fail 2\"\n\
         \x20   if (Count(1).hashCode() != Count(1).hashCode()) return \"fail 3\"\n\
         \x20   if (Count(1).toString() != \"Count(n=1)\") return \"fail 4: ${Count(1)}\"\n\
         \x20   if (Count(21).doubled() != 42) return \"fail 5\"\n\
         \x20   if (Label(\"a\" + \"b\") != Label(\"ab\")) return \"fail 6\"\n\
         \x20   if (\"${Label(\"ab\")}\" != \"Label(text=ab)\") return \"fail 7\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassAnswers", source);
}

/// Into `Any`, a type parameter and generic storage, and back: each crossing boxes as `V`, not as
/// the value inside, and a box read back out is the value again. The storage is the program's own
/// generic classes, so nothing but the representation is under test: a stdlib collection would
/// reach this target's collection paths instead.
#[test]
fn a_value_class_is_boxed_as_itself_where_its_type_is_not_known() {
    let source = "@JvmInline value class Count(val n: Int)\n\
         class Cell<T>(val value: T)\n\
         data class Two<T>(val first: T, val second: T)\n\
         fun <T> identity(value: T): T = value\n\
         fun describe(value: Any): String = when (value) {\n\
         \x20   is Count -> \"count ${value.n}\"\n\
         \x20   is Int -> \"int $value\"\n\
         \x20   else -> \"other\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val any: Any = Count(3)\n\
         \x20   if (describe(any) != \"count 3\") return \"fail 1: ${describe(any)}\"\n\
         \x20   if (any.toString() != \"Count(n=3)\") return \"fail 2: $any\"\n\
         \x20   val back = identity(Count(4))\n\
         \x20   if (back.n != 4) return \"fail 3\"\n\
         \x20   val cast = any as Count\n\
         \x20   if (cast.n != 3) return \"fail 4\"\n\
         \x20   val cell = Cell(Count(2))\n\
         \x20   if (cell.value.n != 2) return \"fail 5\"\n\
         \x20   if (Two(Count(1), Count(2)) != Two(Count(1), Count(2))) return \"fail 6\"\n\
         \x20   if (Two(Count(1), Count(2)) == Two(Count(1), Count(3))) return \"fail 6b\"\n\
         \x20   if ((any as? Count)?.n != 3) return \"fail 7\"\n\
         \x20   if ((\"x\" as Any as? Count) != null) return \"fail 8\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassBoxing", source);
}

/// A nullable occurrence: carried as the value where the value's `null` is free (a `String`),
/// as a box where it is not (an `Int`, which has no `null`).
#[test]
fn a_nullable_value_class_keeps_its_null_apart() {
    let source = "@JvmInline value class Name(val text: String)\n\
         @JvmInline value class Count(val n: Int)\n\
         fun name(present: Boolean): Name? = if (present) Name(\"n\") else null\n\
         fun count(present: Boolean): Count? = if (present) Count(7) else null\n\
         fun box(): String {\n\
         \x20   if (name(true)?.text != \"n\") return \"fail 1\"\n\
         \x20   if (name(false) != null) return \"fail 2\"\n\
         \x20   if (count(true)?.n != 7) return \"fail 3\"\n\
         \x20   if (count(false) != null) return \"fail 4\"\n\
         \x20   val any: Any? = name(true)\n\
         \x20   if (any !is Name) return \"fail 5\"\n\
         \x20   if (count(true).toString() != \"Count(n=7)\") return \"fail 6\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassNullable", source);
}

/// A value class implementing an interface is dispatched through its box, and its own member,
/// which takes the value as `this`, is reached through the bridge that unboxes. The call through
/// the interface is the program's own function, not a stdlib aggregate.
#[test]
fn a_value_class_implements_an_interface_through_its_box() {
    let source = "interface Shape { fun area(): Int }\n\
         @JvmInline value class Square(val side: Int) : Shape {\n\
         \x20   override fun area(): Int = side * side\n\
         \x20   override fun toString(): String = \"Square $side\"\n\
         }\n\
         fun total(a: Shape, b: Shape): Int = a.area() + b.area()\n\
         fun box(): String {\n\
         \x20   if (total(Square(2), Square(3)) != 13) return \"fail 1\"\n\
         \x20   val shape: Shape = Square(4)\n\
         \x20   if (shape.area() != 16) return \"fail 2\"\n\
         \x20   if (shape.toString() != \"Square 4\") return \"fail 3: $shape\"\n\
         \x20   if (Square(5).toString() != \"Square 5\") return \"fail 4\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassInterface", source);
}

/// An `init` block validates the value when it is MADE, and only then: boxing it later runs
/// nothing.
#[test]
fn a_value_class_init_block_runs_when_the_value_is_made() {
    let source = "var made = 0\n\
         @JvmInline value class Positive(val n: Int) {\n\
         \x20   init {\n\
         \x20       require(n > 0) { \"not positive: $n\" }\n\
         \x20       made++\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val p = Positive(1)\n\
         \x20   val boxed: Any = p\n\
         \x20   val again: Any = p\n\
         \x20   if (made != 1) return \"fail 1: $made\"\n\
         \x20   if (boxed != again) return \"fail 2\"\n\
         \x20   val message = try { Positive(-1); \"no throw\" } catch (e: IllegalArgumentException) { e.message }\n\
         \x20   if (message != \"not positive: -1\") return \"fail 3: $message\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassInit", source);
}

/// A value class over a value class is carried as the innermost value, and each level still
/// answers as itself. Checked against kotlinc alone: krusty's JVM backend renders the outer level
/// as `Distance(meters=2.5)`, a divergence of that backend's own.
#[test]
fn a_nested_value_class_is_its_innermost_value() {
    let source = "@JvmInline value class Meters(val value: Double)\n\
         @JvmInline value class Distance(val meters: Meters)\n\
         fun box(): String {\n\
         \x20   val d = Distance(Meters(2.5))\n\
         \x20   if (d.meters.value != 2.5) return \"fail 1\"\n\
         \x20   if (d.toString() != \"Distance(meters=Meters(value=2.5))\") return \"fail 2: $d\"\n\
         \x20   if (Meters(Double.NaN) != Meters(Double.NaN)) return \"fail 3\"\n\
         \x20   if (Meters(0.0) == Meters(-0.0)) return \"fail 4\"\n\
         \x20   return \"OK\"\n\
         }\n";
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "ValueClassNested: kotlinc"
    );
    expect_native_box(source, "ValueClassNested", "OK");
}

/// `T : Int` is carried as a reference, and Kotlin's `+` on it is the primitive operator: the
/// box is opened first. A generic value class over such a `T` is where the corpus found it.
#[test]
fn an_operand_of_a_bounded_type_parameter_is_unboxed() {
    let source = "var total = 0\n\
         fun <T : Int> add(a: T, b: T): Int = a + b\n\
         fun <T : Int> accumulate(v: T) { total += v }\n\
         class Holder<T : Int>(val v: T) {\n\
         \x20   fun twice(): Int = v + v\n\
         }\n\
         @JvmInline value class Wrapped<T : Int>(val v: T) {\n\
         \x20   fun plus(other: Wrapped<T>) = Wrapped(v + other.v)\n\
         }\n\
         fun box(): String {\n\
         \x20   if (add(1, 2) != 3) return \"fail 1\"\n\
         \x20   accumulate(40)\n\
         \x20   accumulate(2)\n\
         \x20   if (total != 42) return \"fail 2\"\n\
         \x20   if (Holder(21).twice() != 42) return \"fail 3\"\n\
         \x20   if (Wrapped(10).plus(Wrapped(20)).v != 30) return \"fail 4\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("BoundedTypeParameterOperand", source);
}

/// Kotlin's unsigned integers are value classes to Kotlin and built-in scalars here, and a value
/// class over one hashes as the unsigned value does. The answers are Kotlin's own: `UByte` and
/// `UShort` hash as `data.toInt()` on the signed type they wrap, which SIGN-extends.
#[test]
fn an_unsigned_field_hashes_as_the_signed_value_it_wraps() {
    let source = "@JvmInline value class Wrapped(val value: UInt)\n\
         data class Row(val a: UByte, val b: UShort, val c: UInt, val d: ULong)\n\
         fun box(): String {\n\
         \x20   if (Wrapped(0u) != Wrapped(0u)) return \"fail 1\"\n\
         \x20   if (Wrapped(7u).hashCode() != Wrapped(7u).hashCode()) return \"fail 2\"\n\
         \x20   if (Wrapped(7u).hashCode() != 7u.hashCode()) return \"fail 3\"\n\
         \x20   if (Wrapped(4294967295u).hashCode() != (4294967295u).hashCode()) return \"fail 4\"\n\
         \x20   val row = Row(255u.toUByte(), 65535u.toUShort(), 4294967295u, 18446744073709551615uL)\n\
         \x20   val same = Row(255u.toUByte(), 65535u.toUShort(), 4294967295u, 18446744073709551615uL)\n\
         \x20   if (row != same) return \"fail 5\"\n\
         \x20   if (row.hashCode() != same.hashCode()) return \"fail 6\"\n\
         \x20   if (row.a.hashCode() != (255u.toUByte()).hashCode()) return \"fail 7\"\n\
         \x20   if (row.b.hashCode() != (65535u.toUShort()).hashCode()) return \"fail 8\"\n\
         \x20   if (row.d.hashCode() != (18446744073709551615uL).hashCode()) return \"fail 9\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "UnsignedHash");
    expect_native_box(source, "UnsignedHash", "OK");
}

/// A full value class has several storage properties and therefore no single unboxed carrier.
/// Native keeps its complete boxed layout while single-field value classes nested around it still
/// follow their ordinary representation policy.
#[test]
fn a_multi_field_value_class_keeps_every_field() {
    let source = "// LANGUAGE: +FullValueClasses\n\
         value class Coordinate(val x: Int, val y: Int)\n\
         value class Wrapped(val coordinate: Coordinate)\n\
         fun box(): String {\n\
         \x20   val point = Coordinate(20, 22)\n\
         \x20   val wrapped = Wrapped(point)\n\
         \x20   if (point.x != 20) return \"fail x: ${point.x}\"\n\
         \x20   if (point.y != 22) return \"fail y: ${point.y}\"\n\
         \x20   if (wrapped.coordinate.x + wrapped.coordinate.y != 42) return \"fail wrapped\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("MultiFieldValueClass", source);
}

/// Kotlin/Native reads a single-field `value class` as inline from the keyword alone. Without
/// `@JvmInline` it answers exactly as an annotated one does, through the value and through a box.
/// kotlinc-native compiles this program and answers "OK" (recorded, or run live where
/// kotlin-native is provisioned); the JVM rejects it.
#[test]
fn a_value_class_needs_no_jvm_inline_on_native() {
    let source = "value class Count(val n: Int) {\n\
         \x20   fun doubled(): Int = n * 2\n\
         }\n\
         value class Label(val text: String)\n\
         fun describe(value: Any): String = when (value) {\n\
         \x20   is Count -> \"count ${value.n}\"\n\
         \x20   else -> \"other\"\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Count(1) != Count(1)) return \"fail 1\"\n\
         \x20   if (Count(1) == Count(2)) return \"fail 2\"\n\
         \x20   if (Count(1).hashCode() != Count(1).hashCode()) return \"fail 3\"\n\
         \x20   if (Count(1).toString() != \"Count(n=1)\") return \"fail 4: ${Count(1)}\"\n\
         \x20   if (Count(21).doubled() != 42) return \"fail 5\"\n\
         \x20   val any: Any = Count(3)\n\
         \x20   if (describe(any) != \"count 3\") return \"fail 6: ${describe(any)}\"\n\
         \x20   val label: Label? = Label(\"ab\")\n\
         \x20   if (\"$label\" != \"Label(text=ab)\") return \"fail 7: $label\"\n\
         \x20   return \"OK\"\n\
         }\n";
    if let Some(answer) = kotlin_native_box(source) {
        assert_eq!(answer, "OK", "kotlinc-native's answer");
    }
    expect_native_box(source, "UnannotatedValueClass", "OK");
}

/// An override whose declared result is a value class answers a call through a generic interface
/// whose result is the type parameter: the call site holds the value class's box, and the default
/// argument stub carries the override's own representation.
#[test]
fn a_value_class_result_reaches_a_generic_caller_through_a_default_argument() {
    let source = "@JvmInline value class Ucn(private val i: UInt)\n\
         interface Input<T> {\n\
         \x20   fun foo(n: Int = 0): T\n\
         }\n\
         class Kx(val x: UInt) : Input<Ucn> {\n\
         \x20   override fun foo(n: Int): Ucn = if (n < 0) Ucn(0u) else Ucn(x)\n\
         }\n\
         fun box(): String {\n\
         \x20   val p = Kx(42u).foo()\n\
         \x20   if (p.toString() != \"Ucn(i=42)\") return \"fail: $p\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("GenericValueClassResult", source);
}

/// A value class's defaulted constructor fills the box the call site allocated, and a defaulted
/// member takes the value as `this` and is called directly: a value class is final.
#[test]
fn a_value_class_fills_its_defaulted_arguments() {
    let source = "@JvmInline value class Z(val x: Int = 1234) {\n\
         \x20   fun test(y: Int = 42) = x + y\n\
         }\n\
         @JvmInline value class S(val x: String = \"foobar\")\n\
         fun box(): String {\n\
         \x20   if (Z().x != 1234) return \"fail 1\"\n\
         \x20   if (S().x != \"foobar\") return \"fail 2\"\n\
         \x20   if (Z(800).test() != 842) return \"fail 3\"\n\
         \x20   if (Z(400).test(32) != 432) return \"fail 4\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassDefaults", source);
}

/// An explicit backing field of a value-class type: the default getter answers the property's
/// public type, so the value is boxed on the way out, while the class reads the field as the value.
#[test]
fn an_explicit_backing_field_of_a_value_class_answers_the_public_type() {
    let source = "// LANGUAGE: +ExplicitBackingFields\n\
         interface I { fun call(): Int }\n\
         @JvmInline value class V(val x: Int) : I { override fun call(): Int = x }\n\
         class A {\n\
         \x20   val a: Any\n\
         \x20       field = V(1)\n\
         \x20   fun bar(): Int = a.call() + a.x\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = A()\n\
         \x20   if (a.bar() != 2) return \"fail 1\"\n\
         \x20   val b: Any = a.a\n\
         \x20   if (b !is V || b.x != 1) return \"fail 2: $b\"\n\
         \x20   if ((b as I).call() != 1) return \"fail 3\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassBackingField", source);
}

/// A dependency interface member whose implementor in this file is a value class taking its own
/// type: the implementor's arm is compiled for every call through the interface, and an operand
/// the site types otherwise reaches it as the reference the interface carries.
#[test]
fn a_value_class_comparable_does_not_capture_another_comparable_call() {
    let source = "@JvmInline value class II(val i: Int) : Comparable<II> {\n\
         \x20   override fun compareTo(other: II): Int = i.compareTo(other.i)\n\
         }\n\
         @JvmInline value class ICmp(val intc: Comparable<Int>)\n\
         fun test(x: ICmp): Int = x.intc.compareTo(42)\n\
         fun box(): String {\n\
         \x20   if (test(ICmp(42)) != 0) return \"fail 1\"\n\
         \x20   val c: Comparable<II> = II(1)\n\
         \x20   if (c.compareTo(II(2)) >= 0) return \"fail 2\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassComparableArm", source);
}

/// A call through `Comparable<Rank>` whose receiver IS a `Rank`: the implementor's arm is the one
/// that runs, and its operand changes representation twice. The site passes the `Rank` value, the
/// dependency declaration's `T` carries it boxed, and `Rank.compareTo` takes the value again. The
/// exact differences catch a box read as the `Long`, and a value read as a box.
#[test]
fn a_comparable_call_selecting_a_value_class_arm_crosses_the_declared_parameter() {
    let source = "@JvmInline value class Rank(val r: Long) : Comparable<Rank> {\n\
         \x20   override fun compareTo(other: Rank): Int = (r - other.r).toInt()\n\
         }\n\
         fun <T : Comparable<T>> order(a: T, b: T): Int = a.compareTo(b)\n\
         fun box(): String {\n\
         \x20   val c: Comparable<Rank> = Rank(10)\n\
         \x20   val direct = c.compareTo(Rank(3))\n\
         \x20   if (direct != 7) return \"fail 1: $direct\"\n\
         \x20   val generic = order(Rank(2), Rank(9))\n\
         \x20   if (generic != -7) return \"fail 2: $generic\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassComparableDeclaredBoundary", source);
}

/// An interface redeclaring an inherited member at a value-class result stands in nothing of its
/// own: the implementor fills both interfaces' numbers, each at its own representation.
#[test]
fn an_abstract_covariant_redeclaration_is_filled_by_the_implementor() {
    let source = "@JvmInline value class X(val x: Any)\n\
         interface IBar { fun bar(): Any }\n\
         interface IFoo : IBar { override fun bar(): X }\n\
         class TestX : IFoo { override fun bar(): X = X(\"K\") }\n\
         fun box(): String {\n\
         \x20   val foo: IFoo = TestX()\n\
         \x20   val bar: IBar = TestX()\n\
         \x20   val viaBar = bar.bar()\n\
         \x20   if (viaBar !is X) return \"fail 1: $viaBar\"\n\
         \x20   if (foo.bar().x != \"K\") return \"fail 2\"\n\
         \x20   if (viaBar.x != \"K\") return \"fail 3\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassCovariantRedeclaration", source);
}

/// A member extension property whose receiver is the interface's type parameter, implemented at
/// a value class: the receiver crosses the interface's number boxed and reaches the
/// implementation as the value.
#[test]
fn a_member_extension_receiver_crosses_a_generic_interface_as_the_value() {
    let source = "@JvmInline value class S(val x: String)\n\
         interface GFoo<T> { val T.extVal: String }\n\
         object GFooImpl : GFoo<S> { override val S.extVal: String get() = x }\n\
         class TestGFoo : GFoo<S> by GFooImpl\n\
         fun <T> read(foo: GFoo<T>, receiver: T): String = with(foo) { receiver.extVal }\n\
         fun box(): String {\n\
         \x20   val direct = with(TestGFoo()) { S(\"O\").extVal }\n\
         \x20   return direct + read(GFooImpl, S(\"K\"))\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassMemberExtension", source);
}

/// A local delegated property of a value-class type reads as the value the generic convention
/// hands back boxed.
#[test]
fn a_local_delegate_of_a_value_class_reads_as_the_value() {
    let source = "import kotlin.reflect.KProperty\n\
         @JvmInline value class ICInt(val i: Int)\n\
         class Delegate<T>(val f: () -> T) {\n\
         \x20   operator fun getValue(thisRef: Any?, property: KProperty<*>): T = f()\n\
         }\n\
         fun box(): String {\n\
         \x20   val local by Delegate { ICInt(44) }\n\
         \x20   if (local.i != 44) return \"fail: $local\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassLocalDelegate", source);
}

/// An `inner` class of a value class holds the outer value, and `this@Outer` reads it back.
#[test]
fn an_inner_class_of_a_value_class_holds_the_outer_value() {
    let source = "@JvmInline value class Z(val x: Int) {\n\
         \x20   @Suppress(\"INNER_CLASS_INSIDE_VALUE_CLASS\")\n\
         \x20   inner class Inner(val y: Int) {\n\
         \x20       val xx = x\n\
         \x20       fun outer(): Z = this@Z\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val inner = Z(42).Inner(100)\n\
         \x20   if (inner.xx != 42) return \"fail 1\"\n\
         \x20   if (inner.y != 100) return \"fail 2\"\n\
         \x20   if (inner.outer() != Z(42)) return \"fail 3: ${inner.outer()}\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassInnerClass", source);
}

/// A collection a value class implements is walked through the value class's own members, which
/// its box reaches through the bridge that unboxes it.
#[test]
fn a_value_class_collection_is_walked_through_its_own_members() {
    let source = "@JvmInline value class Wrapped(val items: List<Int>) : Iterable<Int> {\n\
         \x20   override fun iterator(): Iterator<Int> = items.iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   var sum = 0\n\
         \x20   for (item in Wrapped(listOf(1, 2, 3))) sum += item\n\
         \x20   if (sum != 6) return \"fail 1: $sum\"\n\
         \x20   if (Wrapped(listOf(4, 5)).toList() != listOf(4, 5)) return \"fail 2\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValueClassIterable", source);
}

/// A file boxes and unboxes a value class another file of the module declares: through `Any`, a
/// cast back, and a function value whose parameter the value class is. The declaring file owns the
/// box's layout, so the other file goes through the entry points it defines.
#[test]
fn a_value_class_from_another_file_is_boxed_and_unboxed() {
    let sources: &[(&str, &str)] = &[
        (
            "ValueClassAcrossFiles",
            "fun box(): String {\n\
             \x20   val any: Any = Z(42)\n\
             \x20   if ((any as Z).value != 42) return \"fail 1\"\n\
             \x20   if (any.toString() != \"Z(value=42)\") return \"fail 2: $any\"\n\
             \x20   if (made().value != 7) return \"fail 3\"\n\
             \x20   var seen = 0\n\
             \x20   val take: (Z) -> Unit = { seen = it.value }\n\
             \x20   take(Z(5))\n\
             \x20   if (seen != 5) return \"fail 4\"\n\
             \x20   return \"OK\"\n\
             }\n",
        ),
        (
            "ValueClassAcrossFilesLib",
            "@JvmInline\n\
             value class Z(val value: Int)\n\
             fun made(): Z = (Z(7) as Any) as Z\n",
        ),
    ];
    expect_box_ok_files_with_stdlib(sources, "ValueClassAcrossFiles");
    expect_native_sources(sources, "OK");
}
