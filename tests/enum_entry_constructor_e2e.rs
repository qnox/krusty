//! The constructors of an enum whose constants have bodies, and of those constants' subclasses.
//! An enum's constructors are private. A constant's subclass takes only the name and ordinal: its
//! constructor evaluates the constant's arguments and calls the enum's constructor through the
//! `$default` overload when an argument is omitted, and otherwise through a public synthetic
//! `(…, DefaultConstructorMarker)` accessor the enum declares for it.

use super::common;

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
        assert_eq!(methods(emitted), methods(&reference), "{class}: methods");
    }
}

/// Every method's access flags, name, descriptor and generic `Signature`, in classfile order.
fn methods(bytes: &[u8]) -> Vec<(u16, String, String, Option<String>)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.access,
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
            )
        })
        .collect()
}
