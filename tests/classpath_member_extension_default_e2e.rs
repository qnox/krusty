//! A member extension imported from a dependency object (`import lib.Paths.toPath`) is called
//! through its `$default` bridge when the call omits an argument. The dependency class files come
//! from kotlinc, as a real dependency jar's do.

use super::common;

#[test]
fn an_imported_dependency_member_extension_fills_omitted_defaults() {
    let modules = krusty::conformance::split_modules(include_str!(
        "box_regressions/multimodule/dependencyMemberExtensionDefaults.kt"
    ))
    .expect("the whole member-extension dependency fixture has two modules");
    let [library, main] = modules.as_slice() else {
        panic!("expected the library and main modules");
    };
    assert_eq!((library.files.len(), main.files.len()), (1, 1));
    assert_eq!(
        common::expect_box_run_against_kotlinc(&library.files[0].1, &main.files[0].1)
            .expect("the reference compiler builds the dependency"),
        "OK"
    );
}
