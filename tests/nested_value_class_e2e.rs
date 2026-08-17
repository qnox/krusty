//! A NESTED `@JvmInline value class` must carry the inline-class ABI exactly like a top-level one:
//! `constructor-impl`/`box-impl`/`unbox-impl`, value-based `equals`/`hashCode`, and hash-mangled
//! member names at use sites (`f--MlldnU`). The shared nested-classifier registration funnel
//! previously never set `is_value` (only TOP-LEVEL registration read the `value` modifier), so a
//! nested value class registered as a PLAIN class and miscompiled — a public `<init>`, no impl
//! statics, unmangled member names — for BOTH class and interface owners.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_byte_identical(name: &str, src: &str, class: &str) {
    // `@JvmInline` lives in kotlin-stdlib, so krusty needs it on the classpath (kotlinc adds its
    // own stdlib implicitly).
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => panic!("{name}: reference toolchain unavailable"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// The nested value class itself carries the inline-class ABI. Whole-classfile parity for a value
/// class BODY has a pre-existing member-ORDER divergence from kotlinc (top-level value classes
/// diverge identically), so this asserts the ABI surface instead: the impl statics, the boxing
/// pair, and the `@JvmInline` marker must all be present in the emitted class.
fn assert_value_class_abi(name: &str, src: &str, class: &str) {
    let classes = common::compile_in_process(
        src,
        name,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
    for needle in [
        &b"constructor-impl"[..],
        b"box-impl",
        b"unbox-impl",
        b"equals-impl0",
        b"Lkotlin/jvm/JvmInline;",
    ] {
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "{name}/{class}: inline-class ABI marker {:?} missing",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn interface_nested_value_class_bytes_match_kotlinc() {
    const SRC: &str = "interface I {\n\
        \x20 @JvmInline\n\
        \x20 value class V(val x: Int)\n\
        \x20 fun f(): V?\n\
        }\n";
    // The OWNER is fully byte-identical: the hash-mangled member name (`f--MlldnU`, hashed over the
    // dotted FqName `I.V`) and the name-only `JvmMethodSignature` in `@Metadata` both match kotlinc.
    assert_byte_identical("iface_nested_vc", SRC, "I");
    assert_value_class_abi("iface_nested_vc_inner", SRC, "I$V");
}

#[test]
fn class_nested_value_class_bytes_match_kotlinc() {
    const SRC: &str = "class C {\n\
        \x20 @JvmInline\n\
        \x20 value class V(val x: Int)\n\
        \x20 fun g(): V? = null\n\
        }\n";
    assert_byte_identical("class_nested_vc", SRC, "C");
    assert_value_class_abi("class_nested_vc_inner", SRC, "C$V");
}

#[test]
fn class_nested_value_class_runs() {
    const SRC: &str = "class C {\n\
        \x20 @JvmInline\n\
        \x20 value class V(val x: Int)\n\
        \x20 fun g(): V = V(41)\n\
        }\n\
        fun box(): String {\n\
        \x20 val v = C().g()\n\
        \x20 if (v != C.V(41)) return \"neq\"\n\
        \x20 return if (v.x == 41) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("class-nested value class"), "OK");
}

#[test]
fn interface_nested_value_class_runs() {
    const SRC: &str = "interface I {\n\
        \x20 @JvmInline\n\
        \x20 value class V(val x: Int)\n\
        \x20 fun f(): V\n\
        }\n\
        class Impl : I {\n\
        \x20 override fun f(): I.V = I.V(7)\n\
        }\n\
        fun box(): String {\n\
        \x20 val i: I = Impl()\n\
        \x20 return if (i.f().x == 7) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("interface-nested value class"), "OK");
}
