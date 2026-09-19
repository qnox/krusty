//! An enum's SECONDARY constructors.
//!
//! The enum writer is a separate path from the ordinary class writer, and it emitted none of them.
//! An entry that names one called an `<init>` declared nowhere, so the class failed to load:
//! `NoSuchMethodError: My: method 'void <init>(java.lang.String, int)' not found`. An artifact that
//! could not link, emitted without a diagnostic.
//!
//! Every constructor of a Kotlin enum carries the synthetic `(String name, int ordinal)` ahead of
//! what the declaration wrote. Those slots are forwarded verbatim to a `this(…)` delegation and are
//! not value parameters, so the body's value ids still start at the first declared one — the same
//! split the primary already made, now shared with the secondary path.

use super::common;

fn run(source: &str, stem: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} must compile and run"))
}

/// An entry declared with NO arguments reaching a secondary constructor that delegates to the
/// primary — the corpus's `enum/emptyConstructor.kt`.
#[test]
fn an_entry_may_name_a_secondary_constructor() {
    assert_eq!(
        run(
            "enum class My(val s: String) {\n\
             \x20   ENTRY;\n\
             \x20   constructor(): this(\"OK\")\n\
             }\n\
             \n\
             fun box(): String = My.ENTRY.s\n",
            "EnumSecondaryCtor",
        ),
        "OK"
    );
}

/// Several secondaries beside a primary, each delegating with its own arguments, and entries that
/// pick the primary and a secondary.
#[test]
fn several_secondary_constructors_each_delegate_with_their_own_arguments() {
    assert_eq!(
        run(
            "enum class Test(val x: Int) {\n\
             \x20   A(1),\n\
             \x20   B,\n\
             \x20   C(2, 3);\n\
             \x20   constructor() : this(9)\n\
             \x20   constructor(a: Int, b: Int) : this(a + b)\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (Test.A.x != 1) return \"FAIL1\"\n\
             \x20   if (Test.B.x != 9) return \"FAIL2\"\n\
             \x20   if (Test.C.x != 5) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "EnumSecondaryCtors",
        ),
        "OK"
    );
}

/// With no primary constructor, a body-only enum secondary implicitly initializes the enum base
/// with the compiler-supplied name and ordinal, then runs its body. It must not be dropped merely
/// because there is no source-written delegation expression.
#[test]
fn a_body_only_secondary_constructor_is_emitted() {
    assert_eq!(
        run(
            "enum class Test {\n\
             \x20   A(0), B;\n\
             \n\
             \x20   val n: Int\n\
             \x20   constructor(n: Int) { this.n = n }\n\
             \x20   constructor() : this(0)\n\
             }\n\
             \n\
             fun box(): String = if (Test.A.n == Test.B.n) \"OK\" else \"FAIL\"\n",
            "EnumBodyOnlySecondaryCtor",
        ),
        "OK"
    );
}

/// A bodied entry is emitted as a separate subclass. Its constructor must be able to invoke the
/// exact enum secondary selected for that entry even though Kotlin source constructors are private.
#[test]
fn an_entry_subclass_may_select_a_secondary_constructor() {
    assert_eq!(
        run(
            "enum class Test(val text: String) {\n\
             \x20   A(7) { override fun answer(): String = text },\n\
             \x20   B(\"B\");\n\
             \n\
             \x20   constructor(n: Int) : this(if (n == 7) \"OK\" else \"FAIL\")\n\
             \x20   open fun answer(): String = text\n\
             }\n\
             \n\
             fun box(): String = Test.A.answer()\n",
            "EnumEntrySecondaryCtor",
        ),
        "OK"
    );
}

/// A `vararg` secondary — the corpus's `enum/defaultCtor/secondaryConstructorWithVararg.kt`, where
/// the declared parameter sits after the synthetic prefix.
///
/// `B(4)` deliberately expects `4`, not `1`: a single `Int` argument selects the non-vararg
/// PRIMARY, so only the three-argument entry reaches the secondary. Both compilers agree
/// (`A=3 B=4`), which is what makes the entry a real test of the secondary rather than of overload
/// resolution.
#[test]
fn a_secondary_constructor_may_take_a_vararg() {
    assert_eq!(
        run(
            "enum class Test(val n: Int) {\n\
             \x20   A(1, 2, 3),\n\
             \x20   B(4);\n\
             \x20   constructor(vararg ns: Int) : this(ns.size)\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (Test.A.n != 3) return \"FAIL1\"\n\
             \x20   if (Test.B.n != 4) return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "EnumVarargCtor",
        ),
        "OK"
    );
}

/// An enum with a primary and no secondaries is untouched — the guard that stops a synthesized
/// primary from colliding with a no-argument secondary must not stop an ordinary one.
#[test]
fn an_enum_without_secondary_constructors_is_untouched() {
    assert_eq!(
        run(
            "enum class Colors(val rgb: Int) {\n\
             \x20   RED(0xFF0000),\n\
             \x20   GREEN(0x00FF00);\n\
             }\n\
             \n\
             enum class Plain { A, B }\n\
             \n\
             fun box(): String {\n\
             \x20   if (Colors.RED.rgb != 0xFF0000) return \"FAIL1\"\n\
             \x20   if (Colors.values().size != 2) return \"FAIL2\"\n\
             \x20   if (Plain.valueOf(\"B\") != Plain.B) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "EnumNoSecondary",
        ),
        "OK"
    );
}

/// `javap -v -p` of one class as BOTH compilers emit it: `(kotlinc, krusty)`.
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
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == class)
        .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
    let path = ours_dir.join(format!("{class}.class"));
    std::fs::create_dir_all(path.parent().expect("class parent")).expect("class dir");
    std::fs::write(&path, bytes).expect("write class");

    let dump = |root: &std::path::Path| {
        common::javap(&["-v", "-p", "-cp", &root.to_string_lossy(), class])
            .unwrap_or_else(|| panic!("{stem}: javap failed"))
    };
    (dump(&reference_dir), dump(&ours_dir))
}

/// Every member descriptor, in `javap` order.
fn descriptors(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|line| line.trim().strip_prefix("descriptor: "))
        .map(str::to_string)
        .collect()
}

/// A DEFAULTED enum secondary constructor. kotlinc emits the synthetic overload as a CONSTRUCTOR
/// carrying the owner's prefix first, then the declared parameters, then the mask and the marker:
/// `(Ljava/lang/String;IIILkotlin/jvm/internal/DefaultConstructorMarker;)V` for `constructor(k: Int = 5)`
/// on an enum whose primary is `(String, Int)`. The stub emitter was handed only the CAPTURE prefix,
/// which an enum never has, so it wrote an overload two parameters short — one that every entry
/// omitting the argument calls and no declaration provides.
#[test]
fn a_defaulted_enum_secondary_constructor_matches_kotlinc() {
    let (reference, ours) = javap_both(
        "EnumSecondaryDefault",
        "enum class My(val s: String, val n: Int) {\n\
         \x20   A(\"a\", 1), B(), C(7);\n\
         \x20   constructor(k: Int = 5) : this(\"d\", k)\n\
         }\n",
        "My",
    );
    assert_eq!(descriptors(&ours), descriptors(&reference));
}

/// …and the program runs: an entry that omits the defaulted argument gets the declared default,
/// one that supplies it keeps its own value.
#[test]
fn an_enum_entry_omitting_a_defaulted_secondary_argument_gets_the_default() {
    assert_eq!(
        run(
            "enum class My(val s: String, val n: Int) {\n\
             \x20   A(\"a\", 1), B(), C(7);\n\
             \x20   constructor(k: Int = 5) : this(\"d\", k)\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (My.A.s != \"a\" || My.A.n != 1) return \"FAIL1\"\n\
             \x20   if (My.B.s != \"d\" || My.B.n != 5) return \"FAIL2\"\n\
             \x20   if (My.C.s != \"d\" || My.C.n != 7) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "EnumSecondaryDefaultRun",
        ),
        "OK"
    );
}

/// One exact ledger per constructor member: descriptor, access flags, generic `Signature`,
/// `MethodParameters` and `LineNumberTable`, for the real constructors and the defaulted stub.
fn constructor_ledger(dump: &str, class: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut rows: Vec<String> = Vec::new();
    let mut in_parameters = false;
    for line in dump.lines() {
        let trimmed = line.trim();
        if line.starts_with("  ") && !line.starts_with("   ") && trimmed.ends_with(");") {
            if let Some(header) = current.take() {
                out.push(format!("{header} | {}", rows.join(" | ")));
            }
            rows.clear();
            in_parameters = false;
            // A constructor's javap header names the class itself, never a return type.
            if trimmed.starts_with(class) || trimmed.contains(&format!(" {class}(")) {
                current = Some(trimmed.to_string());
            }
            continue;
        }
        if current.is_none() {
            continue;
        }
        if trimmed == "MethodParameters:" {
            in_parameters = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("descriptor: ") {
            rows.push(format!("descriptor {rest}"));
        } else if let Some(rest) = trimmed.strip_prefix("flags: ") {
            let hex = rest
                .trim_start_matches('(')
                .split(')')
                .next()
                .unwrap_or(rest)
                .to_string();
            rows.push(format!("flags {hex}"));
        } else if let Some(rest) = trimmed.strip_prefix("Signature: ") {
            let value = rest.split_once("// ").map_or(rest, |(_, s)| s).trim();
            rows.push(format!("Signature {value}"));
        } else if let Some(rest) = trimmed.strip_prefix("line ") {
            rows.push(format!("line {rest}"));
        } else if in_parameters && !trimmed.starts_with("Name") && !trimmed.is_empty() {
            let mut fields = trimmed.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(name), flags, None)
                    if name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '$' || c == '_') =>
                {
                    rows.push(format!("param {name}/{}", flags.unwrap_or("-")));
                }
                _ => in_parameters = false,
            }
        }
    }
    if let Some(header) = current.take() {
        out.push(format!("{header} | {}", rows.join(" | ")));
    }
    out
}

/// The full constructor surface of a DEFAULTED enum secondary — one ledger, both compilers.
///
/// Three facts this pins that a descriptor-only comparison cannot: the generic `Signature` is
/// formatted from the SEMANTIC parameter types, so an uncommon fixture-owned `Envelope<String>`
/// keeps its type argument without exercising mapped Kotlin collection handling; the
/// synthetic overload of a PRIVATE constructor is `ACC_SYNTHETIC` alone, never `PUBLIC|SYNTHETIC`;
/// and every `LineNumberTable` entry is the constructor's OWN declaration line, not the primary's.
#[test]
fn a_defaulted_enum_secondary_constructor_ledger_matches_kotlinc() {
    let (reference, ours) = javap_both(
        "EnumSecondaryLedger",
        "class Envelope<T>(val value: T)\n\
         enum class My(val s: String, val n: Int) {\n\
         \x20   A(\"a\", 1), B();\n\
         \x20   constructor(values: Envelope<String> = Envelope(\"d\")) : this(values.value, 1)\n\
         }\n",
        "My",
    );
    assert_eq!(
        constructor_ledger(&ours, "My"),
        constructor_ledger(&reference, "My"),
        "krusty's constructor surface is kotlinc's, entry for entry"
    );
    assert_eq!(
        constructor_ledger(&reference, "My"),
        vec![
            "private My(java.lang.String, int); | descriptor (Ljava/lang/String;ILjava/lang/String;I)V | flags 0x0002 | line 2: 0 | Signature (Ljava/lang/String;I)V".to_string(),
            "private My(Envelope<java.lang.String>); | descriptor (Ljava/lang/String;ILEnvelope;)V | flags 0x0002 | line 4: 0 | Signature (LEnvelope<Ljava/lang/String;>;)V".to_string(),
            "My(java.lang.String, int, Envelope, int, kotlin.jvm.internal.DefaultConstructorMarker); | descriptor (Ljava/lang/String;ILEnvelope;ILkotlin/jvm/internal/DefaultConstructorMarker;)V | flags 0x1000 | line 4: 0".to_string(),
        ],
        "spelled out, so a reference change is visible here"
    );
}

/// The same ledger where every line the constructor owns is a DIFFERENT line: the `constructor`
/// keyword on 2, its parameter's default expression on 4, the `this(…)` delegation on 5.
///
/// This is the fixture that makes the declaration line a fact rather than a coincidence. The
/// synthetic overload enters on line 2, fills the masked parameter on line 4, returns to line 2 for
/// the delegation branch, and delegates on line 5 — so a rule that took the first default
/// expression, or the delegation, or the class, for "the declaration" would put the entry on 4, on
/// 5, or on 1, and each of those is visible here.
///
/// The constructor is `private` for the same reason: a non-private one is entered through a
/// `checkNotNullParameter` prologue krusty does not emit yet, which shifts every pc. That gap is
/// pinned separately, below, so this ledger compares the lines rather than that.
#[test]
fn a_multiline_secondary_constructors_ledger_matches_kotlinc() {
    let (reference, ours) = javap_both(
        "MultilineSecondaryLedger",
        "class Envelope<T>(val value: T)\n\
         class Secret(val s: String, val n: Int) {\n\
         \x20   private constructor(\n\
         \x20       values: Envelope<String> =\n\
         \x20           Envelope(\"d\")\n\
         \x20   ) : this(values.value, 1)\n\
         }\n",
        "Secret",
    );
    assert_eq!(
        constructor_ledger(&ours, "Secret"),
        constructor_ledger(&reference, "Secret"),
        "krusty's constructor surface is kotlinc's, entry for entry"
    );
    assert_eq!(
        constructor_ledger(&reference, "Secret"),
        vec![
            "public Secret(java.lang.String, int); | descriptor (Ljava/lang/String;I)V | flags 0x0001 | line 2: 6".to_string(),
            "private Secret(Envelope<java.lang.String>); | descriptor (LEnvelope;)V | flags 0x0002 | line 6: 0 | Signature (LEnvelope<Ljava/lang/String;>;)V".to_string(),
            "Secret(Envelope, int, kotlin.jvm.internal.DefaultConstructorMarker); | descriptor (LEnvelope;ILkotlin/jvm/internal/DefaultConstructorMarker;)V | flags 0x1000 | line 3: 0 | line 5: 6 | line 3: 16 | line 6: 21".to_string(),
        ],
        "spelled out, so a reference change is visible here"
    );
}

/// A NON-private secondary constructor, where one difference remains and is pinned to its size.
///
/// kotlinc enters such a constructor through `Intrinsics.checkNotNullParameter` — six bytes krusty
/// does not emit for a secondary constructor's parameters — so the single line entry sits at pc 6
/// there and pc 0 here. The LINE is the same on both sides, which is this PR's fact; the pc is the
/// parameter-assertion gap, and the synthetic overload, which has no such prologue, matches exactly.
#[test]
fn a_non_private_secondary_constructor_differs_only_by_its_missing_null_check() {
    let (reference, ours) = javap_both(
        "ProtectedSecondaryLedger",
        "open class Prot(val n: Int) {\n\
         \x20   protected constructor(text: String = \"x\") : this(text.length)\n\
         }\n",
        "Prot",
    );
    assert_eq!(
        constructor_ledger(&reference, "Prot"),
        vec![
            "public Prot(int); | descriptor (I)V | flags 0x0001 | line 1: 0".to_string(),
            "protected Prot(java.lang.String); | descriptor (Ljava/lang/String;)V | flags 0x0004 | line 2: 6".to_string(),
            "public Prot(java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker); | descriptor (Ljava/lang/String;ILkotlin/jvm/internal/DefaultConstructorMarker;)V | flags 0x1001 | line 2: 0".to_string(),
        ],
        "kotlinc's ledger"
    );
    assert_eq!(
        constructor_ledger(&ours, "Prot"),
        vec![
            "public Prot(int); | descriptor (I)V | flags 0x0001 | line 1: 0".to_string(),
            // Same line, pc 0 rather than 6: no `checkNotNullParameter` prologue precedes it.
            "protected Prot(java.lang.String); | descriptor (Ljava/lang/String;)V | flags 0x0004 | line 2: 0".to_string(),
            "public Prot(java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker); | descriptor (Ljava/lang/String;ILkotlin/jvm/internal/DefaultConstructorMarker;)V | flags 0x1001 | line 2: 0".to_string(),
        ],
        "krusty's ledger: access flags and lines are kotlinc's; one pc is not"
    );
}
