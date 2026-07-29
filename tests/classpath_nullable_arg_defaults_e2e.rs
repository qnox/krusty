use super::common;

const LIB: &str = "package lib\n\
     class Tgt(val tag: String)\n\
     interface Eng {\n\
     \x20 fun run(a: String, t: Tgt? = null, extra: String? = null): String\n\
     \x20 suspend fun go(a: String, t: Tgt? = null, extra: String? = null): String\n\
     }\n";

#[test]
fn nullable_argument_with_all_arguments_supplied() {
    let main = "import lib.Eng\n\
        import lib.Tgt\n\
        fun probe(eng: Eng, t: Tgt?): String = eng.run(\"a\", t, \"e\")\n";
    if let Some(diags) = common::checker_diags_against("cpnullargall", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn nullable_argument_with_trailing_default_omitted() {
    let main = "import lib.Eng\n\
        import lib.Tgt\n\
        fun probe(eng: Eng, t: Tgt?): String = eng.run(\"a\", t)\n";
    if let Some(diags) = common::checker_diags_against("cpnullargdefault", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn nullable_argument_to_suspend_member_with_trailing_default_omitted() {
    let main = "import lib.Eng\n\
        import lib.Tgt\n\
        suspend fun probe(eng: Eng, t: Tgt?): String = eng.go(\"a\", t)\n";
    if let Some(diags) = common::checker_diags_against("cpnullargsusp", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn nullable_named_argument_skipping_a_defaulted_parameter() {
    let main = "import lib.Eng\n\
        import lib.Tgt\n\
        fun probe(eng: Eng, t: Tgt?): String = eng.run(\"a\", extra = null, t = t)\n";
    if let Some(diags) = common::checker_diags_against("cpnullargnamed", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn non_null_parameter_still_rejects_a_nullable_argument_with_defaults_omitted() {
    let lib = "package lib\n\
        class Tgt(val tag: String)\n\
        interface Eng {\n\
        \x20 fun run(t: Tgt, extra: String? = null): String\n\
        }\n";
    let main = "import lib.Eng\n\
        import lib.Tgt\n\
        fun probe(eng: Eng, t: Tgt?): String = eng.run(t)\n";
    if let Some(diags) = common::checker_diags_against("cpnullargstrict", lib, main) {
        assert!(
            !diags.is_empty(),
            "a nullable argument for a non-null parameter must be rejected"
        );
    }
}
