//! The constructors of an enum whose constants have bodies, and of those constants' subclasses.
//! An enum's constructors are private. A constant's subclass takes only the name and ordinal: its
//! constructor evaluates the constant's arguments and calls the enum's constructor through the
//! `$default` overload when an argument is omitted, and otherwise through a public synthetic
//! `(…, DefaultConstructorMarker)` accessor the enum declares for it. The constructor a constant
//! selects, a secondary one among them, is the one its subclass calls; the constant's arguments,
//! named, omitted or with side effects, run in the subclass in source order, and the lambdas among
//! them are the subclass's methods, numbered there.

use super::common;
use super::common::expect_native_box;

const ENUMS: &str = "class Cell<T>(val value: T)\n\
enum class Level(val base: Int, val step: Int = 7) {\n\
    LOW(1) { override fun total(): Int = base + step },\n\
    HIGH(2, 3);\n\
    open fun total(): Int = base * step\n\
}\n\
enum class Origin(val size: Int) {\n\
    NEAR(\"ab\") { override fun span(): Int = size + 1 },\n\
    FAR(5);\n\
    constructor(text: String) : this(text.length)\n\
    open fun span(): Int = size\n\
}\n\
enum class Slot(val cell: Cell<String>) {\n\
    FIRST(Cell(\"O\")) { override fun read(): String = cell.value },\n\
    SECOND(Cell(\"K\"));\n\
    open fun read(): String = cell.value\n\
}\n\
fun box(): String {\n\
    val levels = Level.LOW.total() * 10 + Level.HIGH.total()\n\
    val origins = Origin.NEAR.span() * 10 + Origin.FAR.span()\n\
    val slots = Slot.FIRST.read() + Slot.SECOND.read()\n\
    return if (levels == 86 && origins == 35) slots else \"fail\"\n\
}\n";

const CLASSES: [&str; 6] = [
    "Level",
    "Level$LOW",
    "Origin",
    "Origin$NEAR",
    "Slot",
    "Slot$FIRST",
];

const SELECTED: &str = "var counter = 0
fun next(): Int { counter += 1; return counter }
fun tag(): String { counter += 10; return \"n\" }
enum class Plain { A { override fun g() = 1 }, B { override fun g() = 2 }; abstract fun g(): Int }
enum class Labeled(val label: String) {
    A { override fun f() = 1 },
    B(\"y\");
    constructor() : this(\"x\")
    open fun f(): Int = 0
}
enum class Shelf(val width: Int, val label: String = \"w\") {
    WIDE(next()) { override fun f() = 1 },
    NAMED(label = tag(), width = next()) { override fun f() = 2 },
    SPLIT(\"ab\", next()) { override fun f() = 3 },
    TAGGED(\"abc\", 1, \"q\") { override fun f() = 4 },
    PLAIN(9);
    constructor(text: String, extra: Int, tag: String = \"t\") : this(text.length + extra, tag)
    open fun f() = 0
}
enum class Act(val action: () -> Int) {
    ONE({ counter + 1 }) { override fun g() = 1 },
    TWO({ 2 });
    open fun g() = 0
}
fun box(): String {
    if (Plain.A.g() + Plain.B.g() != 3) return \"plain\"
    if (Labeled.A.label != \"x\" || Labeled.A.f() != 1 || Labeled.B.f() != 0) return \"labeled\"
    var shelves = \"\"
    for (shelf in Shelf.values()) shelves += \"${shelf.width}${shelf.label}${shelf.f()},\"
    if (shelves != \"1w1,12n2,15t3,4q4,9w0,\") return shelves
    if (Act.ONE.action() != 14 || Act.ONE.g() != 1 || Act.TWO.action() != 2) return \"act\"
    return \"OK\"
}
";

#[test]
fn a_constant_reaches_the_constructor_it_selects_like_kotlinc() {
    let sources = [("Selected.kt", SELECTED)];
    for class in [
        "Plain",
        "Plain$A",
        "Labeled",
        "Labeled$A",
        "Shelf",
        "Shelf$WIDE",
        "Shelf$NAMED",
        "Shelf$SPLIT",
        "Shelf$TAGGED",
        "SelectedKt",
    ] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert!(pair.krusty == pair.kotlinc, "{class} differs from kotlinc");
    }
    // A constant's lambda argument is its subclass's method, so the enum numbers only its own.
    for class in ["Act", "Act$ONE"] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert_eq!(
            erased_methods(&pair.krusty),
            erased_methods(&pair.kotlinc),
            "{class}: methods"
        );
    }
}

#[test]
fn a_constant_evaluates_its_arguments_once_in_source_order() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SELECTED, "Selected"),
        "OK"
    );
}

#[test]
fn enum_entry_constructors_run() {
    common::expect_box_ok_with_stdlib(ENUMS, "EnumEntryConstructors");
}

#[test]
fn enum_entry_constructors_match_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(ENUMS, "EnumEntryConstructors");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("EnumEntryConstructors.kt");
    std::fs::write(&path, ENUMS).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for class in CLASSES {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            erased_methods(emitted),
            erased_methods(&reference),
            "{class}: methods"
        );
    }
}

/// Every method's access flags, name and descriptor, in classfile order. A generic `Signature` of
/// the enum's own constructor is not this file's subject.
fn erased_methods(bytes: &[u8]) -> Vec<(u16, String, String)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.access,
                method.name.clone(),
                method.descriptor.clone(),
            )
        })
        .collect()
}

const NATIVE: &str = "var counter = 0
fun next(): Int { counter += 1; return counter }
enum class Shelf(val width: Int, val depth: Int = 7) {
    WIDE(next()) { override fun f() = 1 },
    NAMED(depth = next() * 10, width = next()) { override fun f() = 2 },
    SPLIT(100, next(), true) { override fun f() = 3 },
    SHORT(200, true) { override fun f() = 4 },
    PLAIN(9);
    constructor(base: Int, extra: Int, flag: Boolean, scale: Int = 2) : this(base + extra * scale, if (flag) 5 else 6)
    constructor(base: Int, flag: Boolean) : this(base, 0, flag, 3)
    open fun f() = 0
}
fun box(): String {
    var total = 0
    for (shelf in Shelf.values()) total = total * 3 + shelf.width + shelf.depth + shelf.f()
    return if (total == 3091 && counter == 4) \"OK\" else \"fail\"
}
";

/// Native calls the constructor each constant's subclass selected, a secondary one with an
/// omitted default among them, as the JVM does.
#[test]
fn a_constant_reaches_its_selected_constructor_on_native() {
    common::expect_box_ok_with_stdlib(NATIVE, "NativeSelected");
    expect_native_box(NATIVE, "NativeSelected", "OK");
}
