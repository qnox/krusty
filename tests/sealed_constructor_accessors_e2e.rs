//! A sealed class's constructors, reached the way kotlinc reaches them.
//!
//! kotlinc declares every source constructor of a sealed class private in the class file and pairs
//! it with a public synthetic `(…, DefaultConstructorMarker)` accessor. Every call goes through the
//! accessor: a subclass's `super(…)`, the class's own `this(…)`, and the synthetic default-argument
//! constructor. krusty called a secondary constructor, or any constructor from another file,
//! directly, which fails at run time:
//!
//! ```text
//! IllegalAccessError: class Derived tried to access private method 'void Sealed.<init>()'
//! ```
use super::common;

const SHAPES: &str = r#"interface Named<T> {
    fun name(): T
}

sealed class Shape(val label: String) : Named<String> {
    constructor(sides: Int, suffix: String = "gon") : this(suffix + sides)
    private constructor(flag: Boolean) : this("truth")

    override fun name(): String = label

    companion object {
        val count = 3
    }

    class Square : Shape(4)
    class Circle : Shape("circle")
    class Truth : Shape(true)
}

class Triangle : Shape(3, "-")

fun box(): String {
    if (Shape.Square().name() != "gon4") return "square"
    if (Shape.Circle().name() != "circle") return "circle"
    if (Shape.Truth().name() != "truth") return "truth"
    if (Triangle().name() != "-3") return "triangle"
    return if (Shape.count == 3) "OK" else "count"
}
"#;

const BASE: &str = "sealed class Base {\n    class Nested : Base()\n}\n";
const OUTSIDE: &str = "class Outside : Base()\n";
const MAIN: &str = "fun box(): String {\n    \
    val nested: Base = Base.Nested()\n    \
    val outside: Base = Outside()\n    \
    return if (nested is Base.Nested && outside is Outside) \"OK\" else \"Fail\"\n}\n";

#[test]
fn every_sealed_constructor_is_reached_through_its_accessor() {
    common::expect_box_ok_with_stdlib(SHAPES, "SealedShapes");
}

#[test]
fn a_subclass_in_another_file_reaches_the_sealed_accessor() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Base.kt", BASE),
            ("Outside.kt", OUTSIDE),
            ("main.kt", MAIN),
        ],
        "SealedAcrossFiles",
    );
}

/// Each constructor's instructions, with pool indices masked; `javap` names every call target.
fn constructor_code(class: &str, bytes: &[u8], constructor: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("cannot allocate disassembly fixture");
    let path = work.join(format!("{class}.class"));
    std::fs::write(&path, bytes).expect("write class for disassembly");
    let text = common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("javap unavailable");
    let _ = std::fs::remove_dir_all(work);
    let code = common::method_instructions(&text, &format!("{constructor};"));
    assert!(!code.is_empty(), "{class} has no constructor {constructor}");
    code
}

/// The members in classfile order, with their access flags: the accessors follow the bridge, one
/// per constructor in declaration order, ahead of the companion's `access$…$cp`.
#[test]
fn sealed_constructor_accessors_match_kotlinc() {
    let sources = [("Shapes.kt", SHAPES)];
    let members = |bytes: &[u8]| {
        krusty::jvm::classreader::parse_class(bytes)
            .expect("class parses")
            .methods
            .into_iter()
            .map(|method| (method.name, method.descriptor, method.access))
            .collect::<Vec<_>>()
    };
    let shape = common::ModuleClassPair::compile(&sources, "Shape");
    assert_eq!(members(&shape.krusty), members(&shape.kotlinc));
    for constructor in [
        "private Shape(java.lang.String)",
        "private Shape(int, java.lang.String)",
        "public Shape(int, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
        "private Shape(boolean)",
        "public Shape(java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker)",
        "public Shape(int, java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker)",
        "public Shape(boolean, kotlin.jvm.internal.DefaultConstructorMarker)",
    ] {
        assert_eq!(
            constructor_code("Shape", &shape.krusty, constructor),
            constructor_code("Shape", &shape.kotlinc, constructor),
        );
    }
    for (class, constructor) in [
        ("Shape$Square", "public Shape$Square()"),
        ("Shape$Circle", "public Shape$Circle()"),
        ("Shape$Truth", "public Shape$Truth()"),
        ("Triangle", "public Triangle()"),
    ] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert_eq!(
            constructor_code(class, &pair.krusty, constructor),
            constructor_code(class, &pair.kotlinc, constructor),
        );
    }
}
