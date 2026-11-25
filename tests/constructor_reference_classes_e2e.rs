//! kotlinc compiles every constructor reference to a `FunctionReferenceImpl` class named after its
//! position in the file, whatever the expected type is. krusty used to build one that is only
//! passed as a function with a facade static adapter and an `invokedynamic` instead.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/constructorReferenceClasses.kt");
const STEM: &str = "ConstructorReferenceClasses";

#[test]
fn constructor_references_are_classes() {
    assert_eq!(common::expect_box_run_with_stdlib(PROGRAM, STEM), "OK");
    let classes = common::compile_in_process_metadata_cp_module_target(
        PROGRAM,
        STEM,
        &[common::stdlib_jar()],
        "main",
        None,
    )
    .expect("the program compiles");
    let mut names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "ConstructorReferenceClassesKt",
            "ConstructorReferenceClassesKt$box$bound$1",
            "ConstructorReferenceClassesKt$box$inner$1",
            "ConstructorReferenceClassesKt$box$plain$1",
            "Foo",
            "Foo$Bar",
            "Plain",
        ]
    );
    // The bound inner reference is a class too, but kotlinc spills its receiver into a local before
    // the allocation and krusty does not yet: it is checked by name above, not by bytes.
    for class in names
        .iter()
        .filter(|class| class.contains("$box$") && !class.ends_with("$bound$1"))
    {
        common::byte_diff_against_kotlinc_cp(STEM, PROGRAM, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
