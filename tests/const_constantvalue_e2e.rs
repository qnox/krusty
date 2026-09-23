//! `const val` byte-parity with kotlinc: a compile-time-literal `const val` field carries a
//! `ConstantValue` attribute (the JVM initializes it), and when ALL statics are so folded the facade has
//! NO `<clinit>` at all — exactly kotlinc's output (previously krusty emitted no `ConstantValue` and a
//! `<clinit>` with `putstatic`). Verified by parsing the emitted facade class.

use super::common;

use krusty::jvm::classreader::parse_class;

/// The facade's field table as kotlinc emits it: `(access, name, descriptor)` per field, in the
/// reference compiler's own order. Field FLAGS are the whole subject here, so the comparison is
/// exact rather than a per-field probe — a blanket "statics follow the source" would pass every
/// individual assertion about `const` and still get the plain `val` wrong.
fn reference_facade_fields(src: &str, stem: &str) -> Vec<(u16, String, String)> {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-const-reference"));
    std::fs::create_dir_all(&work).expect("create the reference fixture directory");
    let source = work.join("Main.kt");
    std::fs::write(&source, src).expect("write the reference fixture");
    let out = work.join("classes");
    let (code, diagnostics) = common::kotlinc_compile(&[
        "-cp".to_string(),
        common::stdlib_jar().display().to_string(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the fixture: {diagnostics}"
    );
    let bytes = std::fs::read(out.join("MainKt.class")).expect("read the reference facade");
    field_table(&parse_class(&bytes).expect("the reference facade parses"))
}

fn field_table(ci: &krusty::jvm::classreader::ClassInfo) -> Vec<(u16, String, String)> {
    ci.fields
        .iter()
        .map(|field| (field.access, field.name.clone(), field.descriptor.clone()))
        .collect()
}

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

/// The exact field table, against the reference compiler's own. Every facade static shape in one
/// fixture: a private/internal/public `const val` (visibility follows the declaration), a plain
/// `val` and a `var` (always private, reached through accessors), and a `@JvmField` (public by its
/// own rule). Comparing the whole table is what distinguishes the real rule from the two blanket
/// ones that each satisfy half of it.
#[test]
fn the_facade_field_table_matches_the_reference_compiler() {
    const SRC: &str = "private const val HIDDEN = 4\n\
         internal const val SHARED = 5\n\
         const val OPEN = 6\n\
         private val COMPUTED = \"x\"\n\
         internal val ALSO_COMPUTED = \"y\"\n\
         var MUTABLE = 7\n\
         @JvmField val EXPOSED = 8\n\
         fun box() = HIDDEN + SHARED + OPEN + MUTABLE + EXPOSED\n";
    let reference = reference_facade_fields(SRC, "facade_field_table");
    assert_eq!(
        field_table(&facade(SRC)),
        reference,
        "the facade's field table must match kotlinc's exactly"
    );
}
