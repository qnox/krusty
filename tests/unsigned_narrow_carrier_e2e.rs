//! `UByte` and `UShort` constants are carried in the signed primitive their value class wraps.
//!
//! Each is a value class over `Byte`/`Short`, so the JVM carries one in a `B`/`S` slot and 200 as a
//! `UByte` IS the byte `-56`. krusty lowered the constant to the semantic number and never narrowed
//! it, and nothing downstream could: the IR constant records no unsigned type, and the backend
//! derives `Int` from the constant's shape.
//!
//! So an int-sized 200 rode into a `B` parameter. The callee then compared it against the `-56`
//! that `200.toUByte()` produces, and **two equal `UByte` values answered `false`**:
//!
//! ```text
//! krusty   box:  sipush 200     →  b-7apg3OU:(B)     // callee sees 200
//! kotlinc  box:  bipush -56     →  b-7apg3OU:(B)     // callee sees -56
//! ```
//!
//! `toString` hid it, because reading the untruncated 200 as unsigned renders "200" exactly as the
//! correctly narrowed `-56` does. Only a comparison against a value that HAD been narrowed showed
//! the difference, which is why the wrong answer survived the existing unsigned coverage.
//!
//! `UInt` and `ULong` need no narrowing: their carriers are `Int` and `Long`, already the full
//! width of the value.
//!
//! Every expectation here is kotlinc's, taken by compiling and running the same `box()` under it.

use super::common;

fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "C")
}

/// The defect: a narrow unsigned literal reaching a parameter, compared against a narrowed value.
///
/// `two` pins the case that passed by accident before — two unnarrowed values compare equal to
/// each other, so only a comparison against a value that went through `toUByte()` exposed it.
#[test]
fn a_narrow_unsigned_literal_reaches_a_parameter_in_its_carrier() {
    const SRC: &str = "fun b(v: UByte): String = (v == 200.toUByte()).toString()\n\
fun s(v: UShort): String = (v == 40000.toUShort()).toString()\n\
fun two(x: UByte, y: UByte): String = (x == y).toString()\n\
fun box(): String {\n\
    if (b(200u) != \"true\") return \"fail ubyte\"\n\
    if (s(40000u) != \"true\") return \"fail ushort\"\n\
    if (two(200u, 200u) != \"true\") return \"fail pair\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

/// The value each width still reads back as, including both ends of each range.
///
/// Narrowing changes the BITS a constant is carried in, so this is the half that must not move:
/// 255 as a `UByte` is the byte `-1` and still renders "255", and 0 is unchanged. If narrowing
/// were applied to the wrong width — or with the wrong signedness — these are what would break.
#[test]
fn a_narrowed_constant_still_reads_back_as_its_value() {
    const SRC: &str = "fun bv(v: UByte): String = v.toString()\n\
fun sv(v: UShort): String = v.toString()\n\
fun iv(v: UInt): String = v.toString()\n\
fun lv(v: ULong): String = v.toString()\n\
fun box(): String {\n\
    if (bv(0u) != \"0\") return \"fail ubyte min\"\n\
    if (bv(200u) != \"200\") return \"fail ubyte mid: \" + bv(200u)\n\
    if (bv(255u) != \"255\") return \"fail ubyte max: \" + bv(255u)\n\
    if (sv(0u) != \"0\") return \"fail ushort min\"\n\
    if (sv(40000u) != \"40000\") return \"fail ushort mid: \" + sv(40000u)\n\
    if (sv(65535u) != \"65535\") return \"fail ushort max: \" + sv(65535u)\n\
    if (iv(4294967295u) != \"4294967295\") return \"fail uint max\"\n\
    if (lv(18446744073709551615uL) != \"18446744073709551615\") return \"fail ulong max\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

/// The same constants where they are not a bare argument: a collection element and an array.
///
/// These already worked — the value reaches its carrier by another route — and they are here so a
/// later change to the narrowing cannot break them silently.
#[test]
fn a_narrow_unsigned_constant_survives_a_collection_and_an_array() {
    const SRC: &str = "fun box(): String {\n\
    val l = listOf<UByte>(200u, 255u)\n\
    if (l[0].toString() != \"200\" || l[1].toString() != \"255\") return \"fail list\"\n\
    if (l[0] != 200.toUByte()) return \"fail list element\"\n\
    val a = ubyteArrayOf(200u, 255u)\n\
    if (a[0].toString() != \"200\" || a[1].toString() != \"255\") return \"fail array\"\n\
    if (a[0] != 200.toUByte()) return \"fail array element\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

/// Common IR retains the constant's unsigned IDENTITY, and records no carrier.
///
/// This is the phase assertion, and it is the one the behaviour tests above cannot make. They ask
/// what the JVM backend ANSWERS, and an implementation that narrowed inside common lowering
/// answers identically — it was the first version of this change, and every test above passed
/// against it. What that version got wrong is not visible in a running program at all: it decided
/// `UByte` is an `i8` in a phase that `AGENTS.md` says does not choose representation, which is
/// only observable by looking at the IR the backend is handed.
///
/// So: the constant arrives as `UByte(200)` — the VALUE, at its own type — never as `Int(-56)`,
/// which would be the JVM's carrier already chosen, nor as a bare `Int(200)`, which is the
/// identity erased and the carrier question hidden from every backend.
#[test]
fn common_ir_keeps_the_unsigned_identity_and_chooses_no_carrier() {
    use krusty::ir::{IrConst, IrExpr};

    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
    let (files, diagnostics) = common::capture_common_ir(
        "fun b(v: UByte): String = v.toString()\n\
         fun s(v: UShort): String = v.toString()\n\
         fun i(v: UInt): String = v.toString()\n\
         fun box(): String = b(200u) + s(40000u) + i(70000u)\n",
        "Carrier",
        platform,
    );
    assert!(diagnostics.is_empty(), "frontend rejected: {diagnostics:?}");
    let file = files.first().expect("one lowered file");

    let constants: Vec<&IrConst> = file
        .exprs
        .iter()
        .filter_map(|e| match e {
            IrExpr::Const(c) => Some(c),
            _ => None,
        })
        .collect();

    assert!(
        constants.contains(&&IrConst::UByte(200)),
        "a `UByte` constant must reach a backend as UByte(200), not as a chosen carrier; got {constants:?}"
    );
    assert!(
        constants.contains(&&IrConst::UShort(40000)),
        "a `UShort` constant must reach a backend as UShort(40000); got {constants:?}"
    );
    // `UInt`'s carrier is `Int`, already the full width, so no identity is lost by carrying it
    // there and no new form is warranted.
    assert!(
        constants.contains(&&IrConst::Int(70000)),
        "a `UInt` constant stays an `Int`; got {constants:?}"
    );
    // The carrier the JVM backend will choose must NOT already be present.
    assert!(
        !constants.contains(&&IrConst::Int(-56)),
        "common IR must not carry the JVM's narrowed byte; got {constants:?}"
    );
}
