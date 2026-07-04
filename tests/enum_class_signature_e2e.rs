//! Enum class generic `Signature` attribute: kotlinc emits `Signature: Ljava/lang/Enum<LColor;>;`
//! on every `enum class` (the class extends the generic `java.lang.Enum<E>` with `E` bound to itself).
//! krusty must emit the same for bytecode parity — without it, javap shows `extends java.lang.Enum`
//! instead of `extends java.lang.Enum<Color>`. Verified byte-identical to kotlinc in the differential
//! harness (`when/enumOptimization/*`); here we assert krusty's emitted attribute directly.

use super::common;

fn classes(src: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_in_process(src, "E", &[stdlib], Some(&jdk))
}

fn class_sig(cs: &[(String, Vec<u8>)], name: &str) -> Option<String> {
    cs.iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
        .and_then(|ci| ci.signature)
}

#[test]
fn plain_enum_emits_self_bounded_enum_signature() {
    let src = "enum class Color { RED, GREEN, BLUE }\n\
        fun box(): String = if (Color.RED.ordinal == 0) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return; // toolchain unavailable
    };
    assert_eq!(
        class_sig(&cs, "Color").as_deref(),
        Some("Ljava/lang/Enum<LColor;>;"),
        "enum class must carry the Enum<Self> generic signature"
    );
}

#[test]
fn enum_with_members_still_signs() {
    // An enum with a constructor arg + a method still gets the same class-level Enum<Self> signature.
    let src = "enum class Planet(val mass: Int) {\n\
        \x20   EARTH(5), MARS(6);\n\
        \x20   fun heavy(): Boolean = mass > 5\n\
        }\n\
        fun box(): String = if (Planet.MARS.heavy()) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return;
    };
    assert_eq!(
        class_sig(&cs, "Planet").as_deref(),
        Some("Ljava/lang/Enum<LPlanet;>;")
    );
}
