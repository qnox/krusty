//! The intrinsic null check kotlinc inserts when a PLATFORM value (`T!`) is narrowed to an
//! explicitly non-null Kotlin type.
//!
//! A Java call result is `T!`: usable as `T` or `T?`. Where the source commits it to a declared
//! non-null type, kotlinc emits `dup; ldc "<expression>"; invokestatic
//! Intrinsics.checkNotNullExpressionValue`, so a null fails at the boundary where it enters the
//! non-null world instead of far away. Descriptors and `@NotNull` annotations cannot express that:
//! they already agreed while the check was missing, so these tests assert the emitted call sites AND
//! the runtime failure, both against kotlinc 2.4.10.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::common;

/// Every position measured against kotlinc 2.4.10. The guarded ones commit a platform CALL result to
/// a declared non-null type; the controls keep the value flexible (`T!` stays `T!`) or narrow
/// something that is not a call result, and kotlinc leaves those alone.
const POSITIONS: &str = r#"
val topLevelProperty: String = System.getenv("KRUSTY_ABSENT_1")

class Member {
    val memberProperty: String = System.getenv("KRUSTY_ABSENT_2")
}

fun explicitLocal(): Int {
    val local: String = System.getenv("KRUSTY_ABSENT_3")
    return local.length
}

fun takesNonNull(value: String): Int = value.length

fun argument(): Int = takesNonNull(System.getenv("KRUSTY_ABSENT_4"))

fun returned(): String = System.getenv("KRUSTY_ABSENT_5")

fun lambdaParameter(): Int {
    val f: (String) -> Int = { it.length }
    return f(System.getenv("KRUSTY_ABSENT_6"))
}

fun assignment(): Int {
    var v: String = "x"
    v = System.getenv("KRUSTY_ABSENT_7")
    return v.length
}

fun inferredLocal(): Int {
    val local = System.getenv("KRUSTY_ABSENT_8")
    return local.length
}

fun javaMemberReceiver(): Int = System.getenv("KRUSTY_ABSENT_9").length

fun nullableLocal(): Int {
    val local: String? = System.getenv("KRUSTY_ABSENT_10")
    return local?.length ?: 0
}

fun elvis(): String = System.getenv("KRUSTY_ABSENT_11") ?: "d"

fun stringTemplate(): String = "v=${System.getenv("KRUSTY_ABSENT_12")}"

fun conditionalLocal(c: Boolean): Int {
    val v: String = if (c) System.getenv("KRUSTY_ABSENT_13") else "x"
    return v.length
}

fun conditionalReturn(c: Boolean): String = if (c) System.getenv("KRUSTY_ABSENT_14") else "x"

fun whenBranch(k: Int): String = when (k) {
    1 -> System.getenv("KRUSTY_ABSENT_15")
    else -> "x"
}

fun elvisRight(x: String?): Int {
    val v: String = x ?: System.getenv("KRUSTY_ABSENT_16")
    return v.length
}

fun nestedConditional(c: Boolean, d: Boolean): Int {
    val v: String = if (c) { if (d) System.getenv("KRUSTY_ABSENT_17") else "a" } else "b"
    return v.length
}

fun blockBranch(c: Boolean): Int {
    val v: String = if (c) { val t = 1; System.getenv("KRUSTY_ABSENT_18") } else "x"
    return v.length + t()
}

fun t(): Int = 0

fun conditionalArgument(c: Boolean): Int =
    takesNonNull(if (c) System.getenv("KRUSTY_ABSENT_19") else "x")

fun defaulted(value: String = System.getenv("KRUSTY_ABSENT_20")): Int = value.length

fun nullableDefault(value: String? = System.getenv("KRUSTY_ABSENT_21")): Int = value?.length ?: 0

class DefaultedMember(val value: String = System.getenv("KRUSTY_ABSENT_22"))

fun pathDefault(root: java.nio.file.Path = java.nio.file.Paths.get("/")): String = root.toString()

fun box(): String = "OK"
"#;

/// Each guarded position, exercised. A guard that emits but does not fire — or emits code the
/// verifier rejects — is invisible in a disassembly diff, so every position is also run.
const RUNTIME: &str = r#"
class Member {
    val memberProperty: String = System.getenv("KRUSTY_ABSENT_MEMBER")
}

fun takesNonNull(value: String): Int = value.length

fun returned(): String = System.getenv("KRUSTY_ABSENT_RETURN")

fun explicitLocal(): Int {
    val local: String = System.getenv("KRUSTY_ABSENT_LOCAL")
    return local.length
}

fun argument(): Int = takesNonNull(System.getenv("KRUSTY_ABSENT_ARGUMENT"))

fun lambdaParameter(): Int {
    val f: (String) -> Int = { it.length }
    return f(System.getenv("KRUSTY_ABSENT_LAMBDA"))
}

fun assignment(): Int {
    var v: String = "x"
    v = System.getenv("KRUSTY_ABSENT_ASSIGNMENT")
    return v.length
}

fun defaulted(value: String = System.getenv("KRUSTY_ABSENT_DEFAULT")): Int = value.length

fun probe(position: Int): String = try {
    when (position) {
        0 -> returned().length
        1 -> explicitLocal()
        2 -> argument()
        3 -> lambdaParameter()
        4 -> assignment()
        5 -> defaulted()
        else -> Member().memberProperty.length
    }
    "no assertion emitted"
} catch (e: NullPointerException) {
    e.message ?: "no message"
}

fun box(): String {
    var position = 0
    while (position < 7) {
        val message = probe(position)
        if (message != "getenv(...) must not be null") {
            return "position " + position + ": " + message
        }
        position = position + 1
    }
    return "OK"
}
"#;

/// The ticket's repro: an explicitly typed top-level property. Its guard runs in `<clinit>`, so the
/// failure surfaces as an `ExceptionInInitializerError` around the assertion's own exception.
const CLASS_INITIALIZER: &str = r#"
val absent: String = System.getenv("KRUSTY_ABSENT_CLINIT")

fun box(): String = absent
"#;

fn compile_reference(src: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    let work = common::scratch_dir().expect("allocate kotlinc call-assertion fixture");
    let source = work.join(format!("{stem}.kt"));
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, src).expect("write kotlinc call-assertion fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!classes.is_empty(), "kotlinc emitted no classes");
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn collect_classes(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let name = path
                .strip_prefix(root)
                .expect("class below output root")
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((name, std::fs::read(path).expect("read emitted class")));
        }
    }
}

/// Run a compiled fixture's `box()`.
fn run(classes: &[(String, Vec<u8>)]) -> String {
    let box_class = common::find_box_class(classes).expect("no box class emitted");
    let stdlib: Vec<PathBuf> = vec![common::stdlib_jar()];
    common::run_box(classes, &box_class, &stdlib).expect("JVM unavailable")
}

/// Every NAMED guard — `Intrinsics.checkNotNullExpressionValue` — as `(declaring method, message)`. The
/// message is the `ldc` constant the call site consumes — kotlinc derives it from the checked
/// expression (`getenv(...)`), so comparing it pins the spelling. Reading the disassembly rather
/// than the class bytes counts instructions that execute, not leftover constant-pool entries.
fn assertion_sites(classes: &[(String, Vec<u8>)], tag: &str) -> Vec<(String, String)> {
    let work = common::scratch_dir().expect("allocate javap call-assertion fixture");
    let mut arguments = vec!["-p".to_string(), "-c".to_string()];
    for (internal, bytes) in classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create javap input directory");
        }
        std::fs::write(&path, bytes).expect("write javap input");
        arguments.push(path.to_string_lossy().into_owned());
    }
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let disassembly = common::javap(&borrowed).unwrap_or_else(|| panic!("javap failed for {tag}"));
    let _ = std::fs::remove_dir_all(work);

    let mut sites = Vec::new();
    let mut method = String::new();
    let mut last_string: Option<String> = None;
    for line in disassembly.lines() {
        let trimmed = line.trim();
        // A method header sits at javap's declaration indent and ends with `;`. A field declaration
        // does too, so require a parameter list — plus `static {};`, the class initializer that
        // carries a top-level property's guard.
        if !line.starts_with("    ")
            && trimmed.ends_with(';')
            && (trimmed.contains('(') || trimmed == "static {};")
        {
            method = trimmed.to_string();
            last_string = None;
            continue;
        }
        if trimmed.contains("ldc") {
            if let Some(constant) = trimmed.split("// String ").nth(1) {
                last_string = Some(constant.to_string());
            }
        }
        if trimmed.contains("Intrinsics.checkNotNullExpressionValue:") {
            sites.push((
                method.clone(),
                last_string
                    .clone()
                    .unwrap_or_else(|| "<no preceding constant>".to_string()),
            ));
        }
    }
    // Declaration order is a compiler's own business; which method carries which guard is not.
    sites.sort();
    sites
}

struct Builds {
    krusty: Vec<(String, Vec<u8>)>,
    reference: Vec<(String, Vec<u8>)>,
}

fn positions() -> &'static Builds {
    static BUILDS: OnceLock<Builds> = OnceLock::new();
    BUILDS.get_or_init(|| Builds {
        krusty: common::expect_classes_with_stdlib(POSITIONS, "Positions"),
        reference: compile_reference(POSITIONS, "Positions"),
    })
}

#[test]
fn every_guarded_position_fails_where_the_platform_value_enters() {
    let krusty = run(&common::expect_classes_with_stdlib(RUNTIME, "Runtime"));
    assert_eq!(
        krusty, "OK",
        "each narrowing position must throw the reference compiler's exception and message"
    );
    assert_eq!(
        krusty,
        run(&compile_reference(RUNTIME, "Runtime")),
        "the reference compiler must agree that every position fires"
    );
}

#[test]
fn a_top_level_property_fails_from_its_class_initializer() {
    let krusty = run(&common::expect_classes_with_stdlib(
        CLASS_INITIALIZER,
        "ClassInitializer",
    ));
    assert!(
        krusty.contains("NullPointerException:getenv(...) must not be null"),
        "an explicitly typed top-level property must not store null into an @NotNull field: {krusty}"
    );
    assert_eq!(
        krusty,
        run(&compile_reference(CLASS_INITIALIZER, "ClassInitializer")),
        "the reference compiler must fail the same way"
    );
}

#[test]
fn guarded_positions_match_the_reference_compiler() {
    let reference = assertion_sites(&positions().reference, "kotlinc");
    assert!(
        reference.len() >= 7,
        "the fixture must make kotlinc guard every narrowing position: {reference:?}"
    );
    assert_eq!(
        assertion_sites(&positions().krusty, "krusty"),
        reference,
        "krusty must guard the same positions with the same message as kotlinc"
    );
}

/// kotlinc's OTHER form: where the narrowed value has no name to report (a source-block branch,
/// the merged value of a conditional in ARGUMENT position) it emits the message-less
/// `Intrinsics.checkNotNull(Object)V`. krusty emits it for a block branch; the argument-position
/// conditional is still unguarded, and this pins that gap so the assertion fails the day it closes.
#[test]
fn message_less_guards_match_the_reference_compiler_except_argument_conditionals() {
    let nameless = |classes: &[(String, Vec<u8>)], tag| {
        let work = common::scratch_dir().expect("allocate javap fixture");
        let mut arguments = vec!["-p".to_string(), "-c".to_string()];
        for (internal, bytes) in classes {
            let path = work.join(format!("{internal}.class"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create javap input directory");
            }
            std::fs::write(&path, bytes).expect("write javap input");
            arguments.push(path.to_string_lossy().into_owned());
        }
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let text = common::javap(&borrowed).unwrap_or_else(|| panic!("javap failed for {tag}"));
        let _ = std::fs::remove_dir_all(work);
        let mut method = String::new();
        let mut sites = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if !line.starts_with("    ") && trimmed.ends_with(';') && trimmed.contains('(') {
                method = trimmed.to_string();
            } else if trimmed.contains("Intrinsics.checkNotNull:(Ljava/lang/Object;)V") {
                sites.push(method.clone());
            }
        }
        sites
    };
    let reference = nameless(&positions().reference, "kotlinc");
    let argument = "public static final int conditionalArgument(boolean);";
    assert_eq!(
        reference,
        [
            "public static final int blockBranch(boolean);".to_string(),
            argument.to_string(),
        ],
        "the fixture must exercise kotlinc's message-less form in both positions"
    );
    assert_eq!(
        nameless(&positions().krusty, "krusty"),
        ["public static final int blockBranch(boolean);".to_string()],
        "krusty guards the block branch like kotlinc and does not yet guard `{argument}`"
    );
}

/// A Java bean property names the accessor (`getTitle(...)`), a Java field names the field, a
/// Kotlin property names `<get-title>(...)`, and `list[0]` names `get(...)`. The getter expression
/// body guards the Java value inside the getter.
#[test]
fn property_and_index_guards_name_the_producing_callable() {
    let java = [(
        "J.java".to_string(),
        r#"
            public class J {
                public String name = "n";
                public String getTitle() { return "t"; }
            }
        "#
        .to_string(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        panic!("javac must compile the platform-name fixture");
    };
    let source = r#"
        fun field(j: J): String = j.name
        fun bean(j: J): String = j.title
        fun call(j: J): String = j.getTitle()
        fun index(xs: java.util.ArrayList<String>): String = xs[0]
        class Holder(val j: J) {
            val label: String get() = j.title
        }
        class Inferred(val j: J) {
            val title get() = j.title
        }
        fun useInferred(c: Inferred): String = c.title
        fun box(): String = "OK"
    "#;
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classpath = vec![library.clone(), stdlib.clone()];
    let krusty =
        common::expect_compile_in_process(source, "PlatformNames", &classpath, Some(jdk.as_path()));
    let work = common::scratch_dir().expect("allocate kotlinc platform-name fixture");
    let file = work.join("PlatformNames.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&file, source).expect("write platform-name fixture");
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = format!("{}{sep}{}", library.display(), stdlib.display());
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        cp,
        "-nowarn".to_string(),
        file.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let mut reference = Vec::new();
    collect_classes(&output, &output, &mut reference);
    let _ = std::fs::remove_dir_all(&work);
    if let Some(root) = library.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(
        assertion_sites(&krusty, "krusty"),
        assertion_sites(&reference, "kotlinc"),
        "platform guards must name the same callable kotlinc names"
    );
}

/// A Java enum constant is not-null. Returning or storing the repository-owned `E.A` emits no
/// `checkNotNullExpressionValue`. A repository-owned static field of the same enum type that is not
/// an enum constant still does.
#[test]
fn java_enum_constant_is_not_a_platform_value() {
    let java = [
        ("E.java".to_string(), "public enum E { A, B }\n".to_string()),
        (
            "Holder.java".to_string(),
            "public class Holder {\n\
                 public static final E NOT_ENTRY = E.A;\n\
                 public static E mutable = E.A;\n\
             }\n"
            .to_string(),
        ),
    ];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must build the enum-constant fixture");
    let src = "fun explicit(): E = E.A\n\
         val stored: E = E.A\n\
         fun viaLocal(): E {\n\
             val x = E.A\n\
             return x\n\
         }\n\
         fun entryIsNull(): Boolean = E.A == null\n\
         fun ordinaryFinalIsNull(): Boolean = Holder.NOT_ENTRY == null\n\
         fun ordinaryMutableIsNull(): Boolean = Holder.mutable == null\n\
         fun alias(): E = Holder.NOT_ENTRY\n\
         fun mutable(): E = Holder.mutable\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [library.clone(), stdlib.clone(), jdk.clone()];
    let krusty =
        common::expect_compile_in_process(src, "EnumNull", &classpath, Some(jdk.as_path()));
    let work = common::scratch_dir().expect("allocate kotlinc enum-null fixture");
    let file = work.join("EnumNull.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&file, src).expect("write enum-null fixture");
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = format!("{}{sep}{}", library.display(), stdlib.display());
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        cp,
        file.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let mut reference = Vec::new();
    collect_classes(&output, &output, &mut reference);
    let _ = std::fs::remove_dir_all(&work);
    let mut krusty_files = krusty;
    krusty_files.sort_by(|left, right| left.0.cmp(&right.0));
    reference.sort_by(|left, right| left.0.cmp(&right.0));
    let krusty_names: Vec<&str> = krusty_files.iter().map(|(name, _)| name.as_str()).collect();
    let reference_names: Vec<&str> = reference.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(krusty_names, reference_names, "class set");
    for ((name, krusty_bytes), (_, reference_bytes)) in krusty_files.iter().zip(&reference) {
        assert_eq!(
            krusty_bytes,
            reference_bytes,
            "{name} must match kotlinc (krusty {} B, kotlinc {} B)",
            krusty_bytes.len(),
            reference_bytes.len()
        );
    }
}

/// `this(...)` and `super(...)` are value arguments. A platform result committed to a declared
/// non-null parameter is guarded; a type parameter that admits null is not, including after the
/// superclass applies that parameter as a non-null type.
const CONSTRUCTOR_DELEGATION: &str = r#"
class Named(val text: String) {
    constructor(key: String, marker: Int) : this(System.getenv(key))
}

class Entry(val path: java.nio.file.Path) {
    constructor(parent: java.nio.file.Path, child: String) : this(parent.resolve(child))
}

open class Base(val path: java.nio.file.Path)
class Child : Base {
    constructor(parent: java.nio.file.Path, child: String) : super(parent.resolve(child))
}

class Box<T>(val value: T) {
    constructor(items: java.util.List<T>) : this(items.get(0))
}

open class GenericBase<T>(val value: T)
class GenericChild : GenericBase<String> {
    constructor(items: java.util.List<String>) : super(items.get(0))
}

fun box(): String {
    try {
        Named("KRUSTY_ABSENT_CTOR", 0)
        return "missing"
    } catch (e: NullPointerException) {
        val message = e.message
        return if (message == "getenv(...) must not be null") "OK" else message ?: "null"
    }
}
"#;

#[test]
fn constructor_delegation_guards_a_platform_argument() {
    let krusty = common::expect_classes_with_stdlib(CONSTRUCTOR_DELEGATION, "CtorDelegation");
    let reference = compile_reference(CONSTRUCTOR_DELEGATION, "CtorDelegation");
    let class_bytes = |classes: &[(String, Vec<u8>)]| {
        classes
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        class_bytes(&krusty),
        class_bytes(&reference),
        "every emitted constructor-delegation class must be byte-identical to kotlinc"
    );
    assert_eq!(
        assertion_sites(&krusty, "krusty"),
        assertion_sites(&reference, "kotlinc"),
        "constructor delegation must guard the same platform arguments as kotlinc"
    );
    assert_eq!(
        run(&krusty),
        run(&reference),
        "a null platform value must fail at the delegation, with kotlinc's message"
    );
}

#[test]
fn every_guarded_message_is_derived_from_the_checked_call() {
    let sites = assertion_sites(&positions().krusty, "krusty");
    assert!(!sites.is_empty(), "the fixture emitted no guard at all");
    assert!(
        sites.iter()
            .all(|(_, message)| message == "getenv(...)" || message == "get(...)"),
        "every guarded site in the fixture checks a named platform call: {sites:?}"
    );
    assert!(
        sites.iter().any(|(_, message)| message == "get(...)"),
        "a Paths.get default must name the call: {sites:?}"
    );
}
