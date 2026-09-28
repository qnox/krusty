//! A dependency compiled with `-jvm-default=disable` keeps each interface body on the
//! receiver-first `$DefaultImpls` static and leaves the interface method abstract. kotlinc still
//! calls such a member through the interface (`invokeinterface`), letting the implementing class's
//! forwarder dispatch; only a nonvirtual `super` call names the holder static directly.
use super::common;

const LIBRARY: &str = "package legacy\n\
                       interface Shape {\n\
                       \x20   val sides: Int get() = 4\n\
                       \x20   fun area(): Int = 1\n\
                       }\n";

fn legacy_library() -> std::path::PathBuf {
    let work = common::scratch_dir().expect("allocate legacy dependency fixture");
    let output = work.join("library");
    std::fs::create_dir_all(&output).expect("create legacy dependency output");
    let source = work.join("Library.kt");
    std::fs::write(&source, LIBRARY).expect("write legacy dependency source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-jvm-default=disable".to_string(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc legacy dependency failed: {stderr}");
    output
}

/// An ordinary call and an ordinary property read dispatch through the interface.
#[test]
fn an_ordinary_call_dispatches_through_the_legacy_interface() {
    let source = "import legacy.Shape\n\
                  fun measure(shape: Shape): Int = shape.area() + shape.sides\n";
    let compared =
        common::compile_with_kotlinc("Measure", source, &[legacy_library()], &["MeasureKt"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "MeasureKt differs from kotlinc");
}

/// A `super` call and a `super` property read name the holder static.
#[test]
fn a_super_call_names_the_legacy_holder() {
    let source = "import legacy.Shape\n\
                  class Square : Shape {\n\
                  \x20   override val sides: Int get() = super.sides\n\
                  \x20   override fun area(): Int = super.area() + 1\n\
                  }\n";
    let compared = common::compile_with_kotlinc("Square", source, &[legacy_library()], &["Square"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "Square differs from kotlinc");
}
