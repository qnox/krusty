//! kotlinc interns a method's name and descriptor when it visits the method header, before its body
//! interns anything (ASM's `visitMethod`). The compiler-generated methods follow the same rule: a
//! class's `$default` stub, the `$DefaultImpls` holder's forward to the interface's stub, and an
//! interface's `access$…$jd` bridge each put their own name and descriptor into the constant pool
//! ahead of the constants and member references their bodies introduce.
//!
//! Each case asserts that the named class is byte-identical to kotlinc's. The fixtures use neutral
//! names only.
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
