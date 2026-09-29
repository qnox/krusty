//! A captured `var` of a reference type lives in a `Ref.ObjectRef<T>` cell, and kotlinc types every
//! declaration that holds the cell by it: an anonymous object's capture field and constructor
//! parameter, and a lifted local function's parameter, each carry the generic `Signature`
//! `Lkotlin/jvm/internal/Ref$ObjectRef<Ljava/lang/String;>;`. A primitive cell (`Ref$IntRef`) is
//! not generic and signs nothing; a nullable primitive's cell is an `ObjectRef` over its box.
use super::common;
use krusty::jvm::classreader::parse_class;

const SOURCE: &str = r##"fun interface Action {
    fun run()
}

fun anonymous(): String {
    var s = "a"
    val o = object : Action {
        override fun run() {
            s = s + "b"
        }
    }
    o.run()
    return s
}

fun lifted(): String {
    var s = "a"
    fun bump() {
        s = s + "b"
    }
    bump()
    return s
}

fun boxed(): Int? {
    var n: Int? = null
    fun set() {
        n = 1
    }
    set()
    return n
}

fun counted(): Int {
    var c = 0
    fun bump() {
        c = c + 1
    }
    bump()
    return c
}
"##;

const STEM: &str = "ObjectCellSignatures";

/// Every field's and method's name, descriptor and generic `Signature`, in declaration order.
fn declarations(bytes: &[u8]) -> Vec<(String, String, Option<String>)> {
    let class = parse_class(bytes).expect("a parseable class");
    let fields = class
        .fields
        .into_iter()
        .map(|field| (field.name.to_owned(), field.descriptor.to_owned(), field.signature));
    let methods = class
        .methods
        .into_iter()
        .map(|method| (method.name.to_owned(), method.descriptor.to_owned(), method.signature));
    fields.chain(methods).collect()
}

#[test]
fn object_cells_carry_their_element_in_every_signature() {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source_path = dir.join(format!("{STEM}.kt"));
    std::fs::write(&source_path, SOURCE).expect("fixture source");
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp(SOURCE, STEM, &[common::stdlib_jar()])
        .expect("krusty compiles the fixture");
    for class in [
        "ObjectCellSignaturesKt",
        "ObjectCellSignaturesKt$anonymous$o$1",
    ] {
        let expected = std::fs::read(reference.join(format!("{class}.class")))
            .unwrap_or_else(|error| panic!("kotlinc did not emit {class}: {error}"));
        let (_, actual) = classes
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        assert_eq!(
            declarations(actual),
            declarations(&expected),
            "{class}: declarations differ from kotlinc"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A reference to a local function that writes a captured `String` stores the cell in its carrier,
/// whose field and constructor sign `Ref$ObjectRef<Ljava/lang/String;>;`. kotlinc visits the field
/// after every method, so its `Signature` interns after the `invoke` bodies' constants, and the
/// signature's `Ref$ObjectRef` earns the carrier an `InnerClasses` row.
#[test]
fn a_reference_carrier_signs_its_object_cell() {
    let source = r##"fun reference(): String {
    var s = "a"
    fun bump() {
        s = s + "b"
    }
    val r = ::bump
    r()
    return s
}
"##;
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::byte_diff_against_kotlinc_cp(
        "ObjectCellCarrier",
        source,
        "ObjectCellCarrierKt$reference$r$1",
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("the carrier is byte-identical to kotlinc: {error}"));
}
