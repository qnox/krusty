//! `const val` byte-parity with kotlinc: a compile-time-literal `const val` field carries a
//! `ConstantValue` attribute (the JVM initializes it), and when ALL statics are so folded the facade has
//! NO `<clinit>` at all — exactly kotlinc's output (previously krusty emitted no `ConstantValue` and a
//! `<clinit>` with `putstatic`). Verified by parsing the emitted facade class.

use super::common;

use krusty::jvm::classreader::parse_class;

fn facade(src: &str) -> krusty::jvm::classreader::ClassInfo {
    let sl = common::stdlib_jar();
    let jh = common::java_home();
    let jdk = Some(std::path::PathBuf::from(format!("{jh}/lib/modules")));
    let cp: Vec<std::path::PathBuf> = vec![sl];
    let classes =
        common::compile_in_process(src, "Main", &cp, jdk.as_deref()).expect("const file compiles");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n.ends_with("MainKt"))
        .expect("facade class emitted");
    parse_class(bytes).expect("facade parses")
}

#[test]
fn const_field_has_constantvalue_and_no_clinit() {
    let ci = facade("const val X = \"OK\"\nconst val N = 42\nfun box() = X\n");
    let x = ci.fields.iter().find(|f| f.name == "X").expect("X field");
    assert!(
        x.const_value.is_some(),
        "const val X must carry a ConstantValue attribute"
    );
    let n = ci.fields.iter().find(|f| f.name == "N").expect("N field");
    assert!(
        n.const_value.is_some(),
        "const val N must carry a ConstantValue attribute"
    );
    assert!(
        ci.method("<clinit>", "()V").is_none(),
        "an all-const-folded facade must have NO <clinit> (kotlinc emits none)"
    );
}

/// A `const val`'s FIELD visibility follows its declaration; every other facade static is private
/// whatever the source said, because it is reached through accessors.
///
/// krusty published `private const val` as `public static final`, leaking a declaration the source
/// hid — and it was the whole difference on facades whose members and pool already matched
/// kotlinc's. Measured against kotlinc 2.4.10: `private` → 0x001A, `internal` and `public` →
/// 0x0019, since `internal` is a Kotlin boundary with no JVM spelling.
///
/// The plain `private val` beside them pins the other half: its field was already private, and the
/// fix must not be a blanket "statics follow the source".
#[test]
fn a_const_val_field_carries_its_declarations_visibility() {
    let ci = facade(
        "private const val HIDDEN = 4\n\
         internal const val SHARED = 5\n\
         const val OPEN = 6\n\
         private val COMPUTED = \"x\"\n\
         fun box() = HIDDEN + SHARED + OPEN + COMPUTED.length\n",
    );
    let access = |name: &str| -> u16 {
        ci.fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| {
                let names: Vec<&String> = ci.fields.iter().map(|field| &field.name).collect();
                panic!("no {name} field; facade has {names:?}")
            })
            .access
    };
    const PRIVATE_STATIC_FINAL: u16 = 0x001A;
    const PUBLIC_STATIC_FINAL: u16 = 0x0019;
    assert_eq!(access("HIDDEN"), PRIVATE_STATIC_FINAL, "private const val");
    assert_eq!(access("SHARED"), PUBLIC_STATIC_FINAL, "internal const val");
    assert_eq!(access("OPEN"), PUBLIC_STATIC_FINAL, "public const val");
    assert_eq!(
        access("COMPUTED"),
        PRIVATE_STATIC_FINAL,
        "a non-const facade static stays private whatever the source said"
    );
}
