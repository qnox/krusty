//! A bridge method's RETURN adapter.
//!
//! A bridge exists because a supertype's erased signature differs from the override's, and
//! `emit_bridges` adapts both ends: it boxes a primitive argument, `checkcast`s a reference one,
//! converts numeric widths, and boxes a primitive RESULT for a reference-returning supertype. The
//! inverse of that last one was absent in three different shapes, each an artifact emitted with no
//! diagnostic:
//!
//! * the supertype declares a PRIMITIVE while the override hands back the erased generic
//!   `Object` — the bridge pushed the reference and emitted `ireturn`, which the verifier rejects
//!   (`Bad type on operand stack … 'java/lang/Object' is not assignable to integer`);
//! * the supertype declares a VALUE CLASS whose carrier is a REFERENCE — the bridge took the plain
//!   `Object`-to-`String` narrowing and never unboxed, so the carrier was a `ClassCastException`
//!   waiting to happen;
//! * the supertype declares a BUILT-IN UNSIGNED value class — the value-class pass does not lower
//!   those, so the carrier alone selected `java/lang/Number`, which boxed `kotlin.UInt` is not.
//!
//! Every case here is an INSTRUCTION LEDGER compared against the reference compiler, because the
//! shapes differ only in the adapter and a run-only assertion cannot tell them apart: a redundant
//! `checkcast` still verifies and still returns the right answer.

use super::common;

/// Compile with BOTH compilers and return `(kotlinc, krusty)` `javap -c` of one class.
fn javap_both(stem: &str, source: &str, class: &str) -> (String, String) {
    let dir = common::scratch_dir().expect("scratch dir");
    let reference_dir = dir.join(format!("{stem}-ref"));
    let ours_dir = dir.join(format!("{stem}-out"));
    std::fs::create_dir_all(&reference_dir).expect("reference dir");
    std::fs::create_dir_all(&ours_dir).expect("output dir");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc unavailable under the test harness");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp(source, stem, &[common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile"));
    for (name, bytes) in &classes {
        let path = ours_dir.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class parent")).expect("class dir");
        std::fs::write(&path, bytes).expect("write class");
    }
    let dump = |root: &std::path::Path| {
        common::javap(&["-p", "-c", "-cp", &root.to_string_lossy(), class])
            .unwrap_or_else(|| panic!("{stem}: javap failed"))
    };
    (dump(&reference_dir), dump(&ours_dir))
}

/// Every method of the dumped class as `(header, instructions)`, the instructions normalised to
/// `<mnemonic> <symbolic operand>` so constant-pool indices do not enter the comparison.
fn methods(dump: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in dump.lines() {
        let trimmed = line.trim();
        if line.starts_with("  ") && !line.starts_with("   ") && trimmed.ends_with(");") {
            out.push((trimmed.to_string(), Vec::new()));
            continue;
        }
        let Some((offset, rest)) = trimmed.split_once(": ") else {
            continue;
        };
        if !offset.chars().all(|c| c.is_ascii_digit()) || offset.is_empty() {
            continue;
        }
        let Some((_, body)) = out.last_mut() else {
            continue;
        };
        let (mnemonic, operand) = match rest.split_once("// ") {
            Some((code, comment)) => (
                code.split_whitespace().next().unwrap_or("").to_string(),
                comment.trim().to_string(),
            ),
            None => (
                rest.split_whitespace().next().unwrap_or("").to_string(),
                rest.split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        };
        body.push(format!("{mnemonic} {operand}").trim_end().to_string());
    }
    out
}

/// The instructions of the one method whose header contains `needle`.
fn body(dump: &str, needle: &str) -> Vec<String> {
    let found: Vec<_> = methods(dump)
        .into_iter()
        .filter(|(header, _)| header.contains(needle))
        .collect();
    assert_eq!(found.len(), 1, "exactly one method matching {needle:?}");
    found.into_iter().next().expect("method").1
}

fn assert_byte_identical(stem: &str, source: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(stem, source, class, &[common::stdlib_jar()]) {
        Some(Ok(())) => {}
        Some(Err(error)) => panic!("{error}"),
        None => panic!("{stem}: reference toolchain unavailable"),
    }
}

/// The generic base every fixture bridges from: an erased `T` slot behind a non-generic supertype.
const BASE: &str = "open class A<T> {\n\
                    \x20   var slot: T? = null\n\
                    \x20   open val p: T get() = slot!!\n\
                    }\n";

fn primitive_case(stem: &str, kotlin_type: &str, class: &str) -> (Vec<String>, Vec<String>) {
    let source = format!(
        "{BASE}\ninterface C {{ val p: {kotlin_type} }}\nclass {class} : C, A<{kotlin_type}>()\n"
    );
    let (reference, ours) = javap_both(stem, &source, class);
    (body(&reference, "getP()"), body(&ours, "getP()"))
}

/// Every signed primitive return, each compared instruction for instruction. The numerics go
/// through `java/lang/Number`, `Boolean` and `Char` through their own wrappers — a distinction no
/// run-only assertion can see, because `Integer.intValue()` would also return the right answer.
#[test]
fn every_signed_primitive_return_matches_kotlinc() {
    for (index, (kotlin_type, class)) in [
        ("Boolean", "BB"),
        ("Char", "BC"),
        ("Byte", "BY"),
        ("Short", "BS"),
        ("Int", "BI"),
        ("Long", "BL"),
        ("Float", "BF"),
        ("Double", "BD"),
    ]
    .into_iter()
    .enumerate()
    {
        let (want, ours) = primitive_case(&format!("BridgePrim{index}"), kotlin_type, class);
        assert_eq!(ours, want, "{kotlin_type} bridge body");
    }
}

/// A `T : Number` base already returns `()Ljava/lang/Number;` — the owner the unbox is invoked on —
/// so the reference compiler emits NO `checkcast`. A `T : Comparable<T>` bound is not that owner and
/// keeps it. This pair is the reason the ledger has to be exact: a spurious cast still verifies.
#[test]
fn a_bound_decides_whether_the_checkcast_is_emitted() {
    let source = "open class A<T : Number> {\n\
                  \x20   open val p: T = 7 as T\n\
                  }\n\
                  interface C { val p: Int }\n\
                  class B : C, A<Int>()\n\
                  fun box(): String {\n\
                  \x20 val c: C = B()\n\
                  \x20 return if (c.p == 7) \"OK\" else \"FAIL\"\n\
                  }\n";
    let (reference, ours) = javap_both("BridgeNumberBound", source, "B");
    let want = body(&reference, "getP()");
    assert_eq!(
        want,
        vec![
            "aload_0 ".trim_end().to_string(),
            "invokevirtual Method getP:()Ljava/lang/Number;".to_string(),
            "invokevirtual Method java/lang/Number.intValue:()I".to_string(),
            "ireturn ".trim_end().to_string(),
        ],
        "kotlinc's own ledger, spelled out so a reference change is visible here"
    );
    assert_eq!(body(&ours, "getP()"), want, "Number-bound bridge body");
    assert_eq!(run(source, "BridgeNumberBoundRun"), "OK");

    let source = "open class A<T : Comparable<T>> {\n\
                  \x20   open val p: T = 7 as T\n\
                  }\n\
                  interface C { val p: Int }\n\
                  class B : C, A<Int>()\n";
    let (reference, ours) = javap_both("BridgeComparableBound", source, "B");
    let want = body(&reference, "getP()");
    assert!(
        want.iter().any(|row| row.contains("checkcast")),
        "a Comparable bound keeps the cast in kotlinc: {want:?}"
    );
    assert_eq!(body(&ours, "getP()"), want, "Comparable-bound bridge body");
}

/// A user value class with a PRIMITIVE carrier: the carrier comes out of `IC.unbox-impl()I`, not a
/// JVM wrapper.
#[test]
fn a_value_class_with_a_primitive_carrier_unboxes_through_it() {
    let source = "@JvmInline\n\
                  value class IC(val x: Int)\n\
                  \n\
                  abstract class A<T> {\n\
                  \x20   var t: T? = null\n\
                  \x20   fun foo(): T = t!!\n\
                  }\n\
                  \n\
                  interface I { fun foo(): IC }\n\
                  \n\
                  class B : A<IC>(), I\n";
    let (reference, ours) = javap_both("BridgeValueClassInt", source, "B");
    let want = body(&reference, "foo-");
    assert_eq!(
        want,
        vec![
            "aload_0".to_string(),
            "invokevirtual Method foo:()Ljava/lang/Object;".to_string(),
            "checkcast class IC".to_string(),
            "invokevirtual Method IC.\"unbox-impl\":()I".to_string(),
            "ireturn".to_string(),
        ],
        "kotlinc's own ledger, spelled out so a reference change is visible here"
    );
    assert_eq!(body(&ours, "foo-"), want, "value-class bridge body");
}

/// The same with a REFERENCE carrier. This one never reached the unbox at all: `er` is a reference,
/// so the adapter took the ordinary `Object`-to-`String` narrowing and handed the caller a `Text`
/// where a `String` was declared — a `ClassCastException` at the first use.
#[test]
fn a_value_class_with_a_reference_carrier_unboxes_through_it() {
    let source = "@JvmInline\n\
                  value class Text(val x: String)\n\
                  \n\
                  abstract class A<T> {\n\
                  \x20   var t: T? = null\n\
                  \x20   fun foo(): T = t!!\n\
                  }\n\
                  \n\
                  interface I { fun foo(): Text }\n\
                  \n\
                  class B : A<Text>(), I\n";
    let (reference, ours) = javap_both("BridgeValueClassString", source, "B");
    let want = body(&reference, "foo-");
    assert_eq!(
        want,
        vec![
            "aload_0".to_string(),
            "invokevirtual Method foo:()Ljava/lang/Object;".to_string(),
            "checkcast class Text".to_string(),
            "invokevirtual Method Text.\"unbox-impl\":()Ljava/lang/String;".to_string(),
            "areturn".to_string(),
        ],
        "kotlinc's own ledger, spelled out so a reference change is visible here"
    );
    assert_eq!(body(&ours, "foo-"), want, "reference-carrier bridge body");
}

/// A BUILT-IN unsigned value class. The value-class pass deliberately does not lower these, so
/// nothing records the carrier's owner and `er` has already been reduced to the signed carrier —
/// the adapter has to recover the wrapper identity from the SEMANTIC erased return, because boxed
/// `kotlin.UInt` is not a `java.lang.Number`.
///
#[test]
fn an_unsigned_value_class_unboxes_through_its_own_wrapper() {
    for (index, (kotlin_type, value, wrapper, carrier, ret)) in [
        ("UByte", "7u.toUByte()", "kotlin/UByte", "()B", "ireturn"),
        ("UShort", "7u.toUShort()", "kotlin/UShort", "()S", "ireturn"),
        ("UInt", "7u", "kotlin/UInt", "()I", "ireturn"),
        ("ULong", "7uL", "kotlin/ULong", "()J", "lreturn"),
    ]
    .into_iter()
    .enumerate()
    {
        let source = format!(
            "abstract class A<T> {{\n\
             \x20   var value: T? = null\n\
             \x20   fun foo(): T = value!!\n\
             }}\n\
             \n\
             interface I {{ fun foo(): {kotlin_type} }}\n\
             \n\
             class B : A<{kotlin_type}>(), I\n\
             fun box(): String {{\n\
             \x20 val b = B()\n\
             \x20 b.value = {value}\n\
             \x20 val i: I = b\n\
             \x20 return if (i.foo() == {value}) \"OK\" else \"FAIL\"\n\
             }}\n"
        );
        let (reference, ours) = javap_both(&format!("BridgeUnsigned{index}"), &source, "B");
        let want = body(&reference, "foo-");
        assert_eq!(
            want,
            vec![
                "aload_0".to_string(),
                "invokevirtual Method foo:()Ljava/lang/Object;".to_string(),
                format!("checkcast class {wrapper}"),
                format!("invokevirtual Method {wrapper}.\"unbox-impl\":{carrier}"),
                ret.to_string(),
            ],
            "{kotlin_type}: kotlinc's own ledger"
        );
        assert_eq!(
            body(&ours, "foo-"),
            want,
            "{kotlin_type} bridge body and mangled ABI name"
        );
        assert_eq!(
            run(&source, &format!("BridgeUnsignedRun{index}")),
            "OK",
            "{kotlin_type} interface dispatch"
        );
        assert_byte_identical(&format!("BridgeUnsignedBytes{index}"), &source, "B");
    }
}

fn run(source: &str, stem: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} must compile and run"))
}

/// The corpus shape, run: a non-generic interface declares `var size: Int`, a generic base holds
/// `var size: T`. Both accessor bridges are exercised — the setter's argument box was already
/// emitted, the getter's return unbox was not.
#[test]
fn a_getter_bridge_unboxes_the_erased_generic_result() {
    assert_eq!(
        run(
            "open class A<T> {\n\
             \x20   open var size: T = 56 as T\n\
             }\n\
             \n\
             interface C {\n\
             \x20   var size: Int\n\
             }\n\
             \n\
             open class B : C, A<Int>()\n\
             \n\
             fun box(): String {\n\
             \x20   val b = B()\n\
             \x20   if (b.size != 56) return \"FAIL1\"\n\
             \x20   b.size = 55\n\
             \x20   if (b.size != 55) return \"FAIL2\"\n\
             \x20   val c: C = b\n\
             \x20   if (c.size != 55) return \"FAIL3\"\n\
             \x20   c.size = 57\n\
             \x20   if (c.size != 57) return \"FAIL4\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "BridgeGetterUnbox",
        ),
        "OK"
    );
}

/// A parameter AND a return in the same bridge, run: the argument box was already correct; only the
/// result needed the unbox.
#[test]
fn a_function_bridge_unboxes_its_result_and_still_boxes_its_argument() {
    assert_eq!(
        run(
            "open class A<T> {\n\
             \x20   open fun id(x: T): T = x\n\
             }\n\
             \n\
             interface C {\n\
             \x20   fun id(x: Int): Int\n\
             }\n\
             \n\
             class B : C, A<Int>()\n\
             \n\
             fun box(): String {\n\
             \x20   val c: C = B()\n\
             \x20   if (c.id(3) != 3) return \"FAIL1\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "BridgeArgumentAndResult",
        ),
        "OK"
    );
}

/// A user value class, run through the interface: both carriers reach the caller as the carrier the
/// supertype declared, not as the boxed class.
#[test]
fn a_value_class_bridge_runs_through_its_interface() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class IC(val x: Int)\n\
             @JvmInline\n\
             value class Text(val x: String)\n\
             \n\
             abstract class A<T> {\n\
             \x20   var t: T? = null\n\
             \x20   fun foo(): T = t!!\n\
             }\n\
             \n\
             interface I { fun foo(): IC }\n\
             interface J { fun foo(): Text }\n\
             \n\
             class B : A<IC>(), I\n\
             class D : A<Text>(), J\n\
             \n\
             fun box(): String {\n\
             \x20   val b = B()\n\
             \x20   b.t = IC(10)\n\
             \x20   if ((b as I).foo() != IC(10)) return \"FAIL1\"\n\
             \x20   val d = D()\n\
             \x20   d.t = Text(\"s\")\n\
             \x20   if ((d as J).foo() != Text(\"s\")) return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "BridgeValueClassRun",
        ),
        "OK"
    );
}

/// An `Any`-CARRIER value class: `Object` on both sides of the boundary, and still unboxed.
///
/// `@JvmInline value class Ref(val x: Any)` carries `Object`, and the erased `A<T>.foo(): T` returns
/// `Object` too, so a rule that compares the bridge's concrete and erased JVM return types finds
/// them equal and emits nothing — handing the caller the boxed `Ref` where the declaration says the
/// carrier. Which type is unboxed is the value-class pass's answer, not a comparison's.
#[test]
fn an_any_carrier_value_class_is_unboxed_although_both_sides_are_object() {
    let (reference, ours) = javap_both(
        "BridgeAnyCarrier",
        "@JvmInline value class Ref(val x: Any)\n\
         \n\
         interface IRef { fun foo(): Ref }\n\
         open class ARef<T>(private val v: T) { open fun foo(): T = v }\n\
         class BRef : ARef<Ref>(Ref(\"OK\")), IRef\n",
        "BRef",
    );
    assert_eq!(
        body(&ours, "foo-"),
        body(&reference, "foo-"),
        "krusty's bridge body is kotlinc's"
    );
    assert_eq!(
        body(&reference, "foo-"),
        vec![
            "aload_0".to_string(),
            "invokevirtual Method foo:()Ljava/lang/Object;".to_string(),
            "checkcast class Ref".to_string(),
            "invokevirtual Method Ref.\"unbox-impl\":()Ljava/lang/Object;".to_string(),
            "areturn".to_string(),
        ],
        "spelled out, so a reference change is visible here"
    );
}

/// A NULLABLE value class over a carrier that carries null: `unbox-impl` is an instance call, so a
/// legally null result must go past it rather than into it.
///
/// `Text?` with a non-null `String` carrier stays UNBOXED — the carrier itself carries the null —
/// so the bridge's descriptor returns `String` while the delegated generic override may hand back
/// `null`. Invoking `unbox-impl` on that null throws where the declaration says the bridge returns
/// null; kotlinc branches around it, and the runtime half of this test is what the branch is for.
#[test]
fn a_nullable_value_class_return_lets_null_past_its_unbox() {
    let source = "@JvmInline value class Text(val s: String)\n\
                  \n\
                  interface ITextN { fun bar(): Text? }\n\
                  open class ATextN<T> { open fun bar(): T? = null }\n\
                  class BTextN : ATextN<Text>(), ITextN\n";
    let (reference, ours) = javap_both("BridgeNullableCarrier", source, "BTextN");
    let expected = vec![
        "aload_0".to_string(),
        "invokevirtual Method bar:()Ljava/lang/Object;".to_string(),
        "checkcast class Text".to_string(),
        "dup".to_string(),
        "ifnull 17".to_string(),
        "invokevirtual Method Text.\"unbox-impl\":()Ljava/lang/String;".to_string(),
        "goto 19".to_string(),
        "pop".to_string(),
        "aconst_null".to_string(),
        "areturn".to_string(),
    ];
    assert_eq!(body(&reference, "bar-"), expected, "kotlinc's bridge body");
    assert_eq!(body(&ours, "bar-"), expected, "krusty's is the same");

    // …and the interface call really returns null instead of throwing.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::compile_and_run_box(
            &format!(
                "{source}\n\
                 fun box(): String {{\n\
                 \x20   val i: ITextN = BTextN()\n\
                 \x20   return if (i.bar() == null) \"OK\" else \"FAIL\"\n\
                 }}\n"
            ),
            "BridgeNullableCarrierRun",
            &[stdlib],
            Some(jdk.as_path()),
        )
        .as_deref(),
        Some("OK"),
    );
}
