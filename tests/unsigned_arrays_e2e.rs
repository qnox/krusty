//! Kotlin's four unsigned arrays on the JVM.
//!
//! `UIntArray` is `@JvmInline value class UIntArray(private val storage: IntArray)`, so WHERE IT IS
//! USED UNBOXED an unsigned array is the signed array of the same width: `UByteArray` is `byte[]`,
//! `UShortArray` is `short[]`, `UIntArray` is `int[]`, `ULongArray` is `long[]`. The element width
//! decides the allocation, the load and the store opcode, and the array descriptor; only the reading
//! of the bits is unsigned, and that happens above the array.
//!
//! What these do NOT cover is the boxed form. `kotlin.UIntArray` is a real class with `box-impl`
//! and `unbox-impl`, so a value class crossing into `Any` becomes one — and krusty does not box
//! there, which is a separate defect with its own shape, pinned at the end of this file.
//!
//! Only `each_width_allocates_loads_and_stores_through_its_own_opcodes` is evidence about the
//! REPRESENTATION; it reads the bytecode. The rest are runtime semantic controls, and a runtime
//! control cannot prove a representation: a bare `[I` is a nullable JVM reference and survives a
//! generic round trip unchanged, so those programs pass whether or not anything was boxed.

use super::common;

/// Run `body` under krusty AND under the reference compiler, and require `OK` from each.
///
/// Both halves: equality alone would pass if both compilers agreed on the same wrong answer, and an
/// expectation alone would pin my reading rather than Kotlin's.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    assert_eq!(krusty, "OK", "{stem}: krusty");
    assert_eq!(common::kotlinc_box_result(body), "OK", "{stem}: kotlinc");
}

/// krusty's disassembly of `body`'s `box()`, so an ABI or opcode claim is read off the bytecode
/// rather than inferred from what the program printed.
fn disassembled_box(stem: &str, body: &str) -> String {
    let classes = common::expect_compile_in_process(
        body,
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
    // A RUNTIME control, and only that: it pins that `null` and an array are both representable at
    // a nullable unsigned type and that the array's own size survives. It says nothing about the
    // representation — a bare `[I` is itself a nullable JVM reference, so this would pass whether
    // or not the value class is boxed here. What krusty actually does at a reference boundary is
    // the subject of `an_unsigned_array_crossing_into_any_still_diverges_from_kotlinc` below.
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
    // Also a runtime control. A type parameter erases to a reference, and this pins that the
    // elements and length come back intact across that boundary — not that anything was boxed
    // crossing it: a bare `[I` survives `identity<T>` unchanged, so this passes either way.
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

/// The erasure question, recorded as the DIVERGENCE it is rather than deleted.
///
/// An earlier revision asserted only that krusty and kotlinc agree here, which failed and was
/// removed — throwing away the evidence with it. It is kept now as what it always was: a krusty
/// defect this change does not fix, pinned so it cannot be forgotten and so fixing it breaks a test
/// that has to be updated deliberately.
///
/// `kotlin.UIntArray` carries `box-impl`/`unbox-impl`, so a value class crossing into `Any` becomes
/// one and `is IntArray` is false. krusty does not box at that boundary and answers true. Both
/// answers are asserted EXACTLY — `docs/SPEC.md` §6 — because equality alone would pass if the two
/// ever drifted together, and because the point of this test is that they differ.
#[test]
fn an_unsigned_array_crossing_into_any_still_diverges_from_kotlinc() {
    let src = "fun box(): String {\n\
               \x20   val u: Any = UIntArray(1)\n\
               \x20   val b: Any = ubyteArrayOf(1u)\n\
               \x20   return \"\" + (u is IntArray) + \" \" + (u is UIntArray) + \" \" +\n\
               \x20          (u is LongArray) + \" | \" + (b is ByteArray) + \" \" + (b is UByteArray)\n\
               }\n";
    assert_eq!(
        common::kotlinc_box_result(src),
        "false true false | false true",
        "the reference compiler boxes the value class at the `Any` boundary"
    );
    // The NATIVE backend gives each unsigned array its own runtime descriptor, so it answers what
    // the reference compiler answers. The two krusty backends therefore disagree here on purpose,
    // and both answers are named: asserting only that they agree could be satisfied only by making
    // the native one wrong. When the JVM boxing defect is fixed the answers converge, this call
    // fails, and the test comes back to the ordinary helper.
    assert_eq!(
        common::expect_box_run_with_stdlib_diverging(
            src,
            "UnsignedArrayErasureDivergence",
            "false true false | false true",
        ),
        "true true false | true true",
        "krusty's JVM backend does not box there yet — a known defect, not an accepted answer"
    );
}
