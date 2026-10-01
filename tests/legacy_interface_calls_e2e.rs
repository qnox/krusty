//! A dependency compiled with `-jvm-default=disable` keeps each interface body on the
//! receiver-first `$DefaultImpls` static and leaves the interface method abstract. kotlinc still
//! calls such a member through the interface (`invokeinterface`), letting the implementing class's
//! forwarder dispatch; only a nonvirtual `super` call names the holder static directly.
use super::common;

const LIBRARY: &str = "package legacy\n\
                       interface Shape {\n\
                       \x20   val sides: Int get() = 4\n\
                       \x20   fun area(): Int = 1\n\
                       \x20   var label: Int\n\
                       \x20       get() = 0\n\
                       \x20       set(value) {}\n\
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

/// An ordinary write to a legacy `var` dispatches through the interface setter.
#[test]
fn an_ordinary_write_dispatches_through_the_legacy_interface() {
    let source = "import legacy.Shape\n\
                  fun relabel(shape: Shape) { shape.label = 3 }\n";
    let compared =
        common::compile_with_kotlinc("Relabel", source, &[legacy_library()], &["RelabelKt"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "RelabelKt differs from kotlinc");
}

/// A `super` write to a legacy `var` names the holder's setter static.
#[test]
fn a_super_write_names_the_legacy_holder() {
    let source = "import legacy.Shape\n\
                  class Tag : Shape {\n\
                  \x20   override var label: Int\n\
                  \x20       get() = super.label\n\
                  \x20       set(value) { super.label = value }\n\
                  }\n";
    let compared = common::compile_with_kotlinc("Tag", source, &[legacy_library()], &["Tag"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "Tag differs from kotlinc");
}

/// `super@Outer` from an inner class calls the holder static directly with the outer receiver at
/// its own class: the static is public, so the outer class gets no accessor and no `checkcast`.
#[test]
fn an_outer_super_call_from_an_inner_class_names_the_legacy_holder() {
    let source = "import legacy.Shape\n\
                  class Frame : Shape {\n\
                  \x20   inner class Corner {\n\
                  \x20       fun outer(): Int = super@Frame.area()\n\
                  \x20   }\n\
                  }\n";
    let compared = common::compile_with_kotlinc(
        "Frame",
        source,
        &[legacy_library()],
        &["Frame", "Frame$Corner"],
    );
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "Frame differs from kotlinc");
    let (expected, actual) = &compared[1];
    let header = "public final int outer();";
    assert_eq!(
        common::method_block(&disassemble(actual), header),
        common::method_block(&disassemble(expected), header)
    );
}

fn disassemble(bytes: &[u8]) -> String {
    let work = common::scratch_dir().expect("allocate disassembly directory");
    let class = work.join("Corner.class");
    std::fs::write(&class, bytes).expect("write class for disassembly");
    common::javap(&["-c", "-p", "-v", &class.to_string_lossy()]).expect("javap unavailable")
}
