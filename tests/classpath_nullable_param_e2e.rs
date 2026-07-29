use super::common;

const LIB: &str = "package lib\n\
     class Tgt(val tag: String)\n\
     @JvmInline value class Vid(val raw: String)\n\
     class Api {\n\
     \x20 fun plain(t: Tgt?): String = t?.tag ?: \"none\"\n\
     \x20 fun strict(t: Tgt): String = t.tag\n\
     \x20 fun choose(value: Int): String = value.toString()\n\
     \x20 fun choose(value: Tgt?): String = value?.tag ?: \"none\"\n\
     \x20 suspend fun susp(t: Tgt?): String = t?.tag ?: \"none\"\n\
     \x20 fun mangled(id: Vid, t: Tgt?): String = t?.tag ?: id.raw\n\
     }\n";

#[test]
fn nullable_value_passed_to_classpath_member_parameter() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        fun box(): String {\n\
        \x20 val api = Api()\n\
        \x20 val absent: Tgt? = null\n\
        \x20 val present: Tgt? = Tgt(\"T\")\n\
        \x20 if (api.plain(absent) != \"none\") return \"fail absent\"\n\
        \x20 if (api.plain(present) != \"T\") return \"fail present\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnullparam", LIB, main);
}

#[test]
fn nullable_metadata_matches_the_descriptor_overload() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 return if (Api().choose(absent) == \"none\") \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_against("cpnullparamoverload", LIB, main);
}

#[test]
fn nullable_value_passed_to_classpath_suspend_member_parameter() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        suspend fun probe(): String {\n\
        \x20 val api = Api()\n\
        \x20 val absent: Tgt? = null\n\
        \x20 return api.susp(absent)\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnullparamsusp", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn nullable_value_passed_to_value_class_mangled_member_parameter() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        import lib.Vid\n\
        fun box(): String {\n\
        \x20 val api = Api()\n\
        \x20 val absent: Tgt? = null\n\
        \x20 if (api.mangled(Vid(\"v\"), absent) != \"v\") return \"fail absent\"\n\
        \x20 if (api.mangled(Vid(\"v\"), Tgt(\"T\")) != \"T\") return \"fail present\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnullparamvc", LIB, main);
}

#[test]
fn non_null_member_parameter_remains_strict() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        fun box(): String {\n\
        \x20 val absent: Tgt? = null\n\
        \x20 return Api().strict(absent)\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnullparamstrict", LIB, main) {
        assert!(
            diags.iter().any(
                |message| message.starts_with("none of the following candidates is applicable")
            ),
            "{diags:?}"
        );
    }
}

#[test]
fn non_null_argument_to_classpath_member_still_resolves() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        fun box(): String {\n\
        \x20 if (Api().plain(Tgt(\"T\")) != \"T\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnullparamctl", LIB, main);
}

#[test]
fn named_argument_to_classpath_member_still_resolves() {
    let main = "import lib.Api\n\
        import lib.Tgt\n\
        fun box(): String {\n\
        \x20 if (Api().plain(t = Tgt(\"T\")) != \"T\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpnullparamnamed", LIB, main);
}
