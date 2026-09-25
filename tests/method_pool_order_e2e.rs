//! kotlinc's constant pool follows the order its writer visits a method: the header first (ASM's
//! `visitMethod` interns the name and descriptor), then the body in evaluation order, then the
//! local-variable table.
//!
//! - The compiler-generated methods follow the header rule: a class's `$default` stub, the
//!   `$DefaultImpls` holder's forward to the interface's stub, and an interface's `access$…$jd`
//!   bridge each put their own name and descriptor ahead of what their bodies introduce.
//! - A constructor stores a body property after evaluating its initializer, so whatever the
//!   initializer references (a class, a constructor, a lambda) precedes the field's own entries,
//!   and the constructor's `this` follows them.
//!
//! - A primary constructor's descriptor covers every argument, a plain (non-property) parameter
//!   included, and its `$default` overload's body interns before the data-class members that follow.
//!
//! Each case asserts that the named classes are byte-identical to kotlinc's. The fixtures use
//! neutral names only.
use super::common;

/// Compile `src` with kotlinc and with krusty and return both builds of each class in `classes`.
fn build_both(stem: &str, src: &str, classes: &[&str]) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    let source = dir.join(format!("{stem}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let krusty = common::compile_in_process_metadata_cp_module_target(src, stem, &[], "main", None)
        .expect("krusty compiles the fixture");
    let pairs = classes
        .iter()
        .map(|class| {
            let reference = std::fs::read(reference_dir.join(format!("{class}.class")))
                .expect("kotlinc emits the class");
            let ours = krusty
                .iter()
                .find(|(internal, _)| internal == class)
                .map(|(_, bytes)| bytes.clone())
                .expect("krusty emits the class");
            (class.to_string(), reference, ours)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    pairs
}

fn assert_identical(stem: &str, src: &str, classes: &[&str]) {
    for (class, reference, krusty) in build_both(stem, src, classes) {
        assert!(reference == krusty, "{class} differs from kotlinc's build");
    }
}

#[test]
fn a_member_default_stub_interns_its_header_before_its_body() {
    assert_identical(
        "MemberStub",
        "class Meter {\n\
         \x20   fun read(scale: Int, unit: String = \"mm\"): String = unit + scale\n\
         }\n",
        &["Meter"],
    );
}

#[test]
fn a_holder_forward_interns_its_header_before_its_body() {
    assert_identical(
        "HolderForward",
        "interface Dial {\n\
         \x20   fun turn(by: Int = 1): Int = by\n\
         }\n",
        &["Dial$DefaultImpls", "Dial"],
    );
}

#[test]
fn a_property_initializer_interns_its_value_before_its_field() {
    assert_identical(
        "InitializerOrder",
        "class Part\n\
         class Holder(val size: Int) {\n\
         \x20   val part: Part = Part()\n\
         \x20   val action: () -> Int = { 7 }\n\
         \x20   val big: Long = 123456789012L\n\
         }\n",
        &["Part", "Holder"],
    );
}

/// Data-class fields are visited with the other declared fields, after the constructor, so a
/// generic field's `Signature` follows the constructor's `this`.
#[test]
fn a_data_class_generic_field_signature_follows_its_constructor() {
    assert_identical(
        "DataFieldSignature",
        "class Box<T>(val item: T)\n\
         data class Crate(val box: Box<String>)\n",
        &["Box", "Crate"],
    );
}

#[test]
fn a_plain_constructor_parameter_is_in_the_constructor_header() {
    assert_identical(
        "PlainParameter",
        "open class Base(p: Int)\n\
         class Cell<T>(t: T) {\n\
         \x20   var value = t\n\
         }\n",
        &["Base", "Cell"],
    );
}

#[test]
fn a_default_constructor_body_interns_before_data_members() {
    assert_identical(
        "DefaultBody",
        "data class Pair2(val a: Int = 1, val b: String = \"$a\")\n",
        &["Pair2"],
    );
}

/// A plain parameter carries its `@NotNull` parameter annotation, interned with the constructor's
/// header, and its null check runs before the super call.
#[test]
fn a_plain_constructor_parameter_is_annotated_and_checked_before_the_super_call() {
    assert_identical(
        "PlainParameterCheck",
        "open class Base(val s: String)\n\
         class Derived(label: String, n: Int) : Base(label + n)\n",
        &["Base", "Derived"],
    );
}

/// A plain non-null reference parameter interns its `@NotNull` with the constructor's header and
/// its `checkNotNullParameter` name with the constructor's body, although it has no field.
#[test]
fn a_plain_reference_constructor_parameter_is_annotated_with_the_header() {
    assert_identical("PlainReference", "class Label(text: String)\n", &["Label"]);
}

/// A parameter's annotations are placed by its constructor position, not by the position of the
/// stored field that follows it.
#[test]
fn an_annotated_plain_parameter_beside_a_stored_field_keeps_its_position() {
    assert_identical(
        "AnnotatedBesideField",
        "annotation class Mark\n\
         class Tagged(@Mark text: String, val count: Int)\n\
         class Counted(val count: Int, @Mark text: String)\n",
        &["Mark", "Tagged", "Counted"],
    );
}
