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

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

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

/// Into `Any`, a type parameter and a collection, and back: each crossing boxes as `V`, not as
/// the value inside, and a box read back out is the value again.
#[test]
fn a_value_class_is_boxed_as_itself_where_its_type_is_not_known() {
    let source = "@JvmInline value class Count(val n: Int)\n\
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
         \x20   val list = listOf(Count(1), Count(2))\n\
         \x20   if (list[1].n != 2) return \"fail 5\"\n\
         \x20   if (list != listOf(Count(1), Count(2))) return \"fail 6\"\n\
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
/// which takes the value as `this`, is reached through the bridge that unboxes.
#[test]
fn a_value_class_implements_an_interface_through_its_box() {
    let source = "interface Shape { fun area(): Int }\n\
         @JvmInline value class Square(val side: Int) : Shape {\n\
         \x20   override fun area(): Int = side * side\n\
         \x20   override fun toString(): String = \"Square $side\"\n\
         }\n\
         fun total(shapes: List<Shape>): Int = shapes.sumOf { it.area() }\n\
         fun box(): String {\n\
         \x20   if (total(listOf(Square(2), Square(3))) != 13) return \"fail 1\"\n\
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
    let source = "value class Wrapped(val value: UInt)\n\
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

/// `kotlin.Result` is a value class of the stdlib over `Any?`, carried as that reference: a
/// SUCCESS is the value itself and a FAILURE a marker holding the exception, Kotlin's own
/// representation. A `null` success is representable and distinct from a failure, because the
/// marker is never null; that is the case a wrapper-free representation has to get right.
#[test]
fn a_result_is_its_value_or_a_marker_holding_the_exception() {
    let source = "fun box(): String {\n\
         \x20   val ok = Result.success(\"OK\")\n\
         \x20   if (ok.getOrNull() != \"OK\") return \"fail 1\"\n\
         \x20   if (!ok.isSuccess) return \"fail 2\"\n\
         \x20   if (ok.isFailure) return \"fail 3\"\n\
         \x20   if (ok.exceptionOrNull() != null) return \"fail 4\"\n\
         \x20   if (ok.getOrThrow() != \"OK\") return \"fail 5\"\n\
         \x20   val bad = Result.failure<String>(IllegalStateException(\"boom\"))\n\
         \x20   if (bad.getOrNull() != null) return \"fail 6\"\n\
         \x20   if (bad.isSuccess) return \"fail 7\"\n\
         \x20   if (!bad.isFailure) return \"fail 8\"\n\
         \x20   if (bad.exceptionOrNull()?.message != \"boom\") return \"fail 9\"\n\
         \x20   val absent = Result.success<String?>(null)\n\
         \x20   if (absent.isFailure) return \"fail 10\"\n\
         \x20   if (absent.getOrNull() != null) return \"fail 11\"\n\
         \x20   if (absent.exceptionOrNull() != null) return \"fail 12\"\n\
         \x20   val number = Result.success(42)\n\
         \x20   if (number.getOrNull() != 42) return \"fail 13\"\n\
         \x20   if (number.getOrThrow() != 42) return \"fail 14\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ResultValue", source);
}
