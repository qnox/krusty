use super::common;

const LIB: &str = "package lib\n\
    internal class Hidden { fun f(): Int = 1 }\n\
    class Pub(val v: String)\n";

#[test]
fn package_qualified_param_type_resolves_without_import() {
    common::expect_box_ok_against(
        "pkg_qual_type_no_import",
        LIB,
        "fun take(p: lib.Pub): String = p.v\n\
         fun box(): String = take(lib.Pub(\"OK\"))\n",
    );
}

#[test]
fn package_qualified_ctor_call_single_segment_package() {
    common::expect_box_ok_against(
        "pkg_qual_ctor_no_import",
        LIB,
        "fun box(): String {\n\
         \x20 val p = lib.Pub(\"OK\")\n\
         \x20 return p.v\n\
         }\n",
    );
}

#[test]
fn package_qualified_internal_class_is_rejected() {
    let Some(diagnostics) = common::diagnostics_against(
        "pkg_qual_internal_no_import",
        LIB,
        "fun take(h: lib.Hidden): Int = 0\n\
         fun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert_eq!(diagnostics, ["cannot access 'lib.Hidden': it is internal"]);
}
