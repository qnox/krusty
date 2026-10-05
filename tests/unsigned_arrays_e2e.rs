//! Kotlin's four unsigned arrays on the JVM.
//!
//! `UIntArray` is `@JvmInline value class UIntArray(private val storage: IntArray)`, so WHERE IT IS
//! USED UNBOXED an unsigned array is the signed array of the same width: `UByteArray` is `byte[]`,
//! `UShortArray` is `short[]`, `UIntArray` is `int[]`, `ULongArray` is `long[]`. The element width
//! decides the allocation, the load and the store opcode, and the array descriptor; only the reading
//! of the bits is unsigned, and that happens above the array.
//!
//! A use that stays on the array (`return` of `UIntArray`, an index, `size`) keeps that carrier.
//! A use as a reference supertype (`Any`, `Iterable`, a type parameter) boxes through `box-impl`.
//! `is IntArray` on the boxed value is false. `is UIntArray` and the checkcast in front of
//! `unbox-impl` name the box; other casts of the same descriptor name the carrier.

use super::common;

/// Run `body` under krusty AND under the reference compiler, and require `OK` from each.
///
/// Both halves: equality alone would pass if both compilers agreed on the same wrong answer, and an
/// expectation alone would pin my reading rather than Kotlin's.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let body = &opted_in(body);
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    assert_eq!(krusty, "OK", "{stem}: krusty");
    assert_eq!(common::kotlinc_box_result(body), "OK", "{stem}: kotlinc");
}

/// Unsigned arrays are `@ExperimentalUnsignedTypes`; opt in so both compilers accept the fixture
/// without the warning-level opt-in report.
fn opted_in(body: &str) -> String {
    format!("@file:OptIn(ExperimentalUnsignedTypes::class)\n{body}")
}

/// krusty's disassembly of `body`'s `box()`, so an ABI or opcode claim is read off the bytecode
/// rather than inferred from what the program printed.
fn disassembled_box(stem: &str, body: &str) -> String {
    let classes = common::expect_compile_in_process(
        &opted_in(body),
        stem,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let work = common::scratch_dir().expect("scratch dir");
    for (internal, bytes) in &classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, bytes).expect("write class");
    }
    let dumped = common::javap(&[
        "-c",
        "-p",
        "-cp",
        &work.to_string_lossy(),
        &format!("{stem}Kt"),
    ])
    .expect("javap unavailable");
    let _ = std::fs::remove_dir_all(&work);
    dumped
}

#[test]
fn every_unsigned_width_reads_back_what_it_stored() {
    // Each width's top value is the one a SIGNED read of the same bits answers as -1, so a wrong
    // width or a wrong load opcode cannot answer correctly by accident.
    agrees_with_kotlinc(
        "UnsignedArrayRoundTrip",
        "fun box(): String {\n\
         \x20   val bytes = UByteArray(1)\n\
         \x20   val shorts = UShortArray(1)\n\
         \x20   val ints = UIntArray(1)\n\
         \x20   val longs = ULongArray(1)\n\
         \x20   bytes[0] = 255u\n\
         \x20   shorts[0] = 65535u\n\
         \x20   ints[0] = 4294967295u\n\
         \x20   longs[0] = 18446744073709551615uL\n\
         \x20   val text = \"\" + bytes[0] + \" \" + shorts[0] + \" \" + ints[0] + \" \" + longs[0]\n\
         \x20   return if (text == \"255 65535 4294967295 18446744073709551615\") \"OK\"\n\
         \x20          else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_keeps_its_own_element_width() {
    // `size` and a second element prove the allocation is the right WIDTH rather than merely wide
    // enough: a `UByteArray` laid out as `int[]` still answers element 0 correctly.
    agrees_with_kotlinc(
        "UnsignedArrayWidth",
        "fun box(): String {\n\
         \x20   val bytes = UByteArray(2)\n\
         \x20   bytes[0] = 1u\n\
         \x20   bytes[1] = 255u\n\
         \x20   val shorts = UShortArray(2)\n\
         \x20   shorts[0] = 1u\n\
         \x20   shorts[1] = 65535u\n\
         \x20   val text = \"${bytes.size} ${bytes[0]} ${bytes[1]} ${shorts.size} ${shorts[1]}\"\n\
         \x20   return if (text == \"2 1 255 2 65535\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_literal_carries_its_elements() {
    agrees_with_kotlinc(
        "UnsignedArrayLiteral",
        "fun box(): String {\n\
         \x20   val values = ubyteArrayOf(1u, 255u)\n\
         \x20   val longs = ulongArrayOf(18446744073709551615uL)\n\
         \x20   val text = \"${values[0]} ${values[1]} ${longs[0]}\"\n\
         \x20   return if (text == \"1 255 18446744073709551615\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_walks_its_elements_unsigned() {
    // Iteration reads through a different path than an indexed read, so a right index and a wrong
    // walk would still print negatives.
    agrees_with_kotlinc(
        "UnsignedArrayWalk",
        "fun box(): String {\n\
         \x20   val values = ubyteArrayOf(255u, 7u)\n\
         \x20   var text = \"\"\n\
         \x20   for (value in values) text += \"$value \"\n\
         \x20   return if (text == \"255 7 \") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn each_width_allocates_loads_and_stores_through_its_own_opcodes() {
    // The claim this change makes is about BYTECODE, and a program that prints the right number
    // cannot prove which opcodes produced it — `int[]` holds a 255 perfectly well. So the four
    // shapes are read off the disassembly: the `newarray` operand, the store, the load, and the
    // local's descriptor.
    for (kind, atype, store, load, returned) in [
        ("UByteArray", "byte", "bastore", "baload", "byte[] box()"),
        ("UShortArray", "short", "sastore", "saload", "short[] box()"),
        ("UIntArray", "int", "iastore", "iaload", "int[] box()"),
        ("ULongArray", "long", "lastore", "laload", "long[] box()"),
    ] {
        let stem = format!("Opcodes{kind}");
        let dumped = disassembled_box(
            &stem,
            &format!(
                "fun box(): {kind} {{\n\
                 \x20   val values = {kind}(1)\n\
                 \x20   values[0] = values[0]\n\
                 \x20   return values\n\
                 }}\n"
            ),
        );
        for expected in [
            format!("newarray       {atype}"),
            store.into(),
            load.into(),
            returned.into(),
        ] {
            assert!(
                dumped.contains(&expected),
                "{kind}: {expected:?} missing from\n{dumped}"
            );
        }
    }
}

#[test]
fn a_nullable_unsigned_array_carries_both_null_and_a_value() {
    // `UIntArray?` stays the carrier: null and a value are both representable, and `size` reads
    // the array. Boxing happens when the value is consumed as a supertype, not because the local
    // is nullable.
    agrees_with_kotlinc(
        "NullableUnsignedArray",
        "fun box(): String {\n\
         \x20   val present: UIntArray? = UIntArray(1)\n\
         \x20   val absent: UIntArray? = null\n\
         \x20   if (absent != null) return \"fail: null became a value\"\n\
         \x20   val size = present?.size ?: -1\n\
         \x20   return if (size == 1) \"OK\" else \"fail: $size\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_through_a_generic_keeps_its_elements() {
    // A type parameter erases to a reference, so the argument is boxed on the way in and unboxed
    // when the result is used as the array again.
    agrees_with_kotlinc(
        "GenericUnsignedArray",
        "fun <T> identity(value: T): T = value\n\
         fun box(): String {\n\
         \x20   val values = identity(ubyteArrayOf(1u, 255u))\n\
         \x20   val text = \"${values[0]} ${values[1]} ${values.size}\"\n\
         \x20   return if (text == \"1 255 2\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_array_of_unsigned_is_not_an_unsigned_array() {
    // `Array<UInt>` is a REFERENCE array of boxed `UInt`s; `UIntArray` is an `int[]`. Two different
    // types that the element type alone would conflate, which is exactly what the width rule must
    // not do.
    agrees_with_kotlinc(
        "ArrayOfUnsignedVersusUnsignedArray",
        "fun box(): String {\n\
         \x20   val boxed: Array<UInt> = arrayOf(1u, 4294967295u)\n\
         \x20   val packed: UIntArray = uintArrayOf(1u, 4294967295u)\n\
         \x20   val text = \"${boxed[1]} ${packed[1]} ${boxed.size} ${packed.size}\"\n\
         \x20   return if (text == \"4294967295 4294967295 2 2\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn a_user_value_class_over_an_array_keeps_its_own_carrier() {
    // The unsigned arrays are stdlib value classes over arrays, and nothing about the rule is
    // special to the stdlib: a user's own value class over an array carries the same way.
    agrees_with_kotlinc(
        "UserValueClassOverArray",
        "@JvmInline\n\
         value class Packed(val storage: ByteArray)\n\
         fun box(): String {\n\
         \x20   val packed = Packed(byteArrayOf(1, -1))\n\
         \x20   val text = \"${packed.storage[0]} ${packed.storage[1]} ${packed.storage.size}\"\n\
         \x20   return if (text == \"1 -1 2\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

/// `kotlin.UIntArray` carries `box-impl`/`unbox-impl`. Crossing into `Any` boxes, so `is IntArray`
/// is false and `is UIntArray` is true. A bare `int[]` would answer the opposite on the first test.
#[test]
fn an_unsigned_array_boxes_when_used_as_any() {
    agrees_with_kotlinc(
        "UnsignedArrayBoxedAsAny",
        "fun box(): String {\n\
         \x20   val u: Any = UIntArray(1)\n\
         \x20   val b: Any = ubyteArrayOf(1u)\n\
         \x20   val text = \"\" + (u is IntArray) + \" \" + (u is UIntArray) + \" \" +\n\
         \x20          (u is LongArray) + \" | \" + (b is ByteArray) + \" \" + (b is UByteArray)\n\
         \x20   return if (text == \"false true false | false true\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

/// `Iterable.forEach` is a reference consumer. The carrier is boxed, then `iterator()` runs on
/// that box; indexing the same array stays on the carrier.
#[test]
fn an_unsigned_array_for_each_boxes_before_iteration() {
    agrees_with_kotlinc(
        "UnsignedArrayForEach",
        "fun box(): String {\n\
         \x20   var i = 0\n\
         \x20   val a = ubyteArrayOf(3u, 2u, 1u)\n\
         \x20   var text = \"\"\n\
         \x20   a.forEach { e -> text += \"$e\"; if (e == a[i]) i++ }\n\
         \x20   return if (text == \"321\" && i == 3) \"OK\" else \"fail: $text $i\"\n\
         }\n",
    );
}

/// A file-level function that takes an unsigned array is mangled, and the parameter stays the
/// carrier. kotlinc's declaration for this exact signature is `take-GBYM_sE(byte[])`.
#[test]
fn an_unsigned_array_parameter_is_mangled() {
    let body = "fun take(a: UByteArray): Int = a.size\n\
         fun box(): String {\n\
         \x20   val n = take(ubyteArrayOf(1u, 2u))\n\
         \x20   return if (n == 2) \"OK\" else \"fail: $n\"\n\
         }\n";
    agrees_with_kotlinc("UnsignedArrayMangle", body);
    let dumped = disassembled_box("UnsignedArrayMangle", body);
    let declaration = dumped
        .lines()
        .find(|line| line.contains("take-GBYM_sE"))
        .unwrap_or("")
        .trim();
    assert_eq!(
        declaration, "public static final int take-GBYM_sE(byte[]);",
        "mangled declaration missing from\n{dumped}"
    );
}

/// `val buffer: UByteArray` stores `byte[]`. `buffer.copyOf` must not checkcast that carrier to
/// `kotlin.UByteArray` before reading it.
#[test]
fn an_unsigned_array_property_is_already_the_carrier() {
    agrees_with_kotlinc(
        "UnsignedArrayProperty",
        "class Holder(val buffer: UByteArray) {\n\
         \x20   fun grown(): UByteArray = buffer.copyOf(buffer.size)\n\
         }\n\
         fun box(): String {\n\
         \x20   val values = Holder(ubyteArrayOf(1u)).grown()\n\
         \x20   return if (values.size == 1 && values[0] == 1u.toUByte()) \"OK\" else \"fail\"\n\
         }\n",
    );
    if common::corpus_ready() {
        assert_eq!(
            common::run_box_corpus_case("bridges/test25.kt").as_deref(),
            Some("OK")
        );
    }
}

/// A value-class member that returns an unsigned array returns the carrier, not a box to unbox
/// at the call.
#[test]
fn a_value_class_member_returns_an_unsigned_array_carrier() {
    agrees_with_kotlinc(
        "ValueClassUnsignedArrayReturn",
        "@JvmInline\n\
         value class Foo(val x: Int) {\n\
         \x20   fun arr(): UByteArray = ubyteArrayOf(x.toUByte())\n\
         }\n\
         fun box(): String {\n\
         \x20   val values = Foo(2).arr()\n\
         \x20   return if (values.size == 1 && values[0] == 2u.toUByte()) \"OK\" else \"fail\"\n\
         }\n",
    );
}

/// A generic override specialized to `UByteArray` returns the carrier. Checkcasting that `byte[]`
/// to `kotlin.UByteArray` is the bridge bug: the value is not the box.
#[test]
fn a_generic_unsigned_array_bridge_returns_the_carrier() {
    agrees_with_kotlinc(
        "UnsignedArrayBridge",
        "abstract class Base<T> {\n\
         \x20   abstract fun make(): T\n\
         \x20   fun read(): T = make()\n\
         }\n\
         object Bytes : Base<UByteArray>() {\n\
         \x20   override fun make(): UByteArray = ubyteArrayOf(255u)\n\
         }\n\
         fun box(): String {\n\
         \x20   val values = Bytes.read()\n\
         \x20   return if (values.size == 1 && values[0] == 255u.toUByte()) \"OK\" else \"fail\"\n\
         }\n",
    );
}

/// Annotation instantiation passes the carrier into the impl constructor. The value-class marker
/// accessor is not part of that ABI.
#[test]
fn an_annotation_unsigned_array_uses_the_carrier_constructor() {
    agrees_with_kotlinc(
        "UnsignedArrayAnnotation",
        "// LANGUAGE: +InstantiationOfAnnotationClasses\n\
         annotation class Ann(val array: UIntArray)\n\
         annotation class Var(vararg val foo: UInt)\n\
         fun box(): String {\n\
         \x20   if (Ann(uintArrayOf()) != Ann(uintArrayOf())) return \"fail ann\"\n\
         \x20   Var()\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

/// `Array<UIntArray>` stores boxed `kotlin.UIntArray` values, so a cast to `Array<Any?>` can read
/// them back. An `int[][]` allocation rejects that store. Indexing the element unboxes to the
/// carrier before the inner load.
#[test]
fn an_array_of_unsigned_arrays_stores_the_box() {
    agrees_with_kotlinc(
        "ArrayOfUnsignedArray",
        "fun box(): String {\n\
         \x20   val values: Array<UIntArray> = Array(2) { uintArrayOf(it.toUInt()) }\n\
         \x20   val raw = values as Array<Any?>\n\
         \x20   val back = raw as Array<UIntArray>\n\
         \x20   val text = \"${raw.size} ${back[1][0]}\"\n\
         \x20   return if (text == \"2 1\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

/// The corpus file `initializers/static_arrays.kt`: every primitive array and `arrayOf` form
/// prints through `joinToString`, including the four unsigned widths.
#[test]
fn static_arrays_join_to_string_matches_the_corpus() {
    if !common::corpus_ready() {
        return;
    }
    assert_eq!(
        common::run_box_corpus_case("initializers/static_arrays.kt").as_deref(),
        Some("OK")
    );
}

/// `joinToString` walks an unsigned array as `Iterable`, which boxes, then prints the unsigned
/// elements.
#[test]
fn unsigned_array_join_to_string_prints_unsigned_elements() {
    agrees_with_kotlinc(
        "UnsignedArrayJoin",
        "fun box(): String {\n\
         \x20   val text = uintArrayOf(13u, 14u, 4294967295u).joinToString()\n\
         \x20   val bytes = ubyteArrayOf(20u, 21u, 200u).joinToString()\n\
         \x20   return if (text == \"13, 14, 4294967295\" && bytes == \"20, 21, 200\") \"OK\"\n\
         \x20          else \"fail: $text | $bytes\"\n\
         }\n",
    );
}

/// A nullable unsigned array returned as `Any?` boxes on the non-null path and stays null otherwise.
/// `is UIntArray` is then true and `is IntArray` is false; the carrier alone answers the opposite.
#[test]
fn a_nullable_unsigned_array_boxes_when_returned_as_any() {
    agrees_with_kotlinc(
        "NullableUnsignedArrayAsAny",
        "fun take(a: UIntArray?): Any? = a\n\
         fun box(): String {\n\
         \x20   val present = take(UIntArray(1))\n\
         \x20   val absent = take(null)\n\
         \x20   val text = \"\" + (present is UIntArray) + \" \" + (present is IntArray) + \" \" +\n\
         \x20          (absent == null)\n\
         \x20   return if (text == \"true false true\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}
