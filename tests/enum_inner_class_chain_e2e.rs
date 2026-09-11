//! A nested ENUM omitted the ENCLOSING rows of its `InnerClasses` table.
//!
//! kotlinc lists every class on the nesting chain, not only the referenced class itself: an
//! `enum class Phase` declared inside `Outer.Middle` gets a row for `Outer$Middle` as well as its
//! own. The enum emit path never registered the file's nest — it returns before the classifier
//! path's registration — so its table was assembled from constant-pool lookups alone, which see the
//! enum and its `Companion` but never the enclosing class no member happens to name.
//!
//! It is a structural difference, not a cosmetic one: reflection reads `getSimpleName`/
//! `getEnclosingClass` off this table, so a chain with a hole reports the wrong nesting.
//!
//! A one-level nest cannot show it (the single enclosing row IS the referenced class's outer, which
//! the enum's own row already interns), which is why the existing enum fixtures missed it.
use super::common;

const SRC: &str = "class Outer {\n\
                   \x20   class Middle {\n\
                   \x20       enum class Phase { ONE }\n\
                   \x20   }\n\
                   }\n";

/// The `InnerClasses` entries of `class` as `(inner, outer, simple name, access)`, in table order.
fn inner_classes(bytes: &[u8]) -> Vec<(String, Option<String>, Option<String>, u16)> {
    krusty::jvm::classreader::parse_class(bytes)
        .expect("emitted class parses")
        .inner_classes
        .iter()
        .map(|entry| {
            (
                entry.inner.clone(),
                entry.outer.clone(),
                entry.name.clone(),
                entry.access,
            )
        })
        .collect()
}

/// Compile `SRC` with both compilers; return `(kotlinc bytes, krusty bytes)` for one class.
fn build_both(class: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).ok()?;
    let source = dir.join("EnumInnerChain.kt");
    std::fs::write(&source, SRC).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let reference = std::fs::read(reference_dir.join(format!("{class}.class"))).ok()?;
    let classes = common::compile_in_process_metadata_cp_module_target(
        SRC,
        "EnumInnerChain",
        &[],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    let (_, krusty) = classes
        .iter()
        .find(|(name, _)| name == class)
        .unwrap_or_else(|| panic!("krusty did not emit {class}"));
    let krusty = krusty.clone();
    let _ = std::fs::remove_dir_all(&dir);
    Some((reference, krusty))
}

#[test]
fn a_nested_enum_lists_its_whole_enclosing_chain() {
    let Some((reference, krusty)) = build_both("Outer$Middle$Phase") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let want = inner_classes(&reference);
    assert!(
        want.iter().any(|(inner, ..)| inner == "Outer$Middle"),
        "reference must carry the enclosing row — the rule under test: {want:?}"
    );
    assert_eq!(inner_classes(&krusty), want, "InnerClasses entries");
}
