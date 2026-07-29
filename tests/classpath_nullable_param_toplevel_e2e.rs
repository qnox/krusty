use super::common;

const LIB: &str = "package lib\n\
     class Tgt(val tag: String)\n\
     class Holder(val label: String)\n\
     fun describe(t: Tgt?): String = t?.tag ?: \"none\"\n\
     fun Holder.decorate(t: Tgt?): String = label + (t?.tag ?: \"none\")\n\
     fun blend(first: Tgt, second: Tgt?): String = first.tag + (second?.tag ?: \"none\")\n\
     fun Holder.mix(first: Tgt, second: Tgt?): String = label + first.tag + (second?.tag ?: \"none\")\n\
     suspend fun awaitDescribe(t: Tgt?): String = t?.tag ?: \"none\"\n\
";

#[test]
fn nullable_value_passed_to_classpath_top_level_parameter() {
    let main = "import lib.Tgt\n\
        import lib.describe\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 val present: Tgt? = Tgt(\"T\")\n\
        \x20 if (describe(absent) != \"none\") return \"fail absent\"\n\
        \x20 if (describe(present) != \"T\") return \"fail present\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnulltoplevel", LIB, main);
}

#[test]
fn nullable_value_passed_to_classpath_extension_parameter() {
    let main = "import lib.Holder\n\
        import lib.Tgt\n\
        import lib.decorate\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 if (Holder(\"h\").decorate(absent) != \"hnone\") return \"fail absent\"\n\
        \x20 if (Holder(\"h\").decorate(Tgt(\"T\")) != \"hT\") return \"fail present\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnullextension", LIB, main);
}

#[test]
fn nullable_value_passed_to_suspend_top_level_parameter() {
    let main = "import lib.Tgt\n\
        import lib.awaitDescribe\n\
        suspend fun probe(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 return awaitDescribe(absent)\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnullsuspendtoplevel", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn non_null_top_level_parameter_still_rejects_a_nullable_argument() {
    let main = "import lib.Tgt\n\
        import lib.blend\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 return blend(absent, Tgt(\"T\"))\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnulltoplevelstrict", LIB, main) {
        assert!(
            !diags.is_empty(),
            "a nullable argument for a non-null top-level parameter must be rejected"
        );
    }
}

#[test]
fn non_null_extension_parameter_still_rejects_a_nullable_argument() {
    let main = "import lib.Holder\n\
        import lib.Tgt\n\
        import lib.mix\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 return Holder(\"h\").mix(absent, Tgt(\"T\"))\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnullextstrict", LIB, main) {
        assert!(
            !diags.is_empty(),
            "a nullable argument for a non-null extension parameter must be rejected"
        );
    }
}

#[test]
fn nullable_second_parameter_of_an_extension_is_accepted() {
    let main = "import lib.Holder\n\
        import lib.Tgt\n\
        import lib.mix\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 if (Holder(\"h\").mix(Tgt(\"T\"), absent) != \"hTnone\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnullextsecond", LIB, main);
}

#[test]
fn nullable_second_parameter_of_a_top_level_function_is_accepted() {
    let main = "import lib.Tgt\n\
        import lib.blend\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 if (blend(Tgt(\"T\"), absent) != \"Tnone\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnulltoplevelsecond", LIB, main);
}
