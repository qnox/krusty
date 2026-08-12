use super::common;

#[test]
fn inapplicable_local_candidate_does_not_hide_top_level_candidate() {
    const SRC: &str = "fun choose(value: String): String = \"OK\"\n\
        fun box(): String {\n\
        \x20 fun choose(value: Int): String = \"local\"\n\
        \x20 return choose(\"value\")\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("top-level fallback"),
        "OK"
    );
}

#[test]
fn applicable_local_candidate_wins_before_top_level_candidates() {
    const SRC: &str = "fun choose(value: Any): String = \"top-level\"\n\
        fun box(): String {\n\
        \x20 fun choose(value: Int): String = \"OK\"\n\
        \x20 return choose(1)\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("local tower level"),
        "OK"
    );
}

#[test]
fn local_overload_selects_the_unique_most_specific_candidate() {
    const SRC: &str = "fun box(): String {\n\
        \x20 fun choose(value: Any): String = \"any\"\n\
        \x20 fun choose(value: Int): String = \"OK\"\n\
        \x20 return choose(1)\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("specific local overload"),
        "OK"
    );
}

#[test]
fn local_overload_scores_named_arguments_in_parameter_order() {
    const SRC: &str = "fun box(): String {\n\
        \x20 fun choose(a: Int, b: String): String = \"OK\"\n\
        \x20 fun choose(a: String, b: Int): String = \"wrong\"\n\
        \x20 return choose(b = \"K\", a = 1)\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("named local overload"),
        "OK"
    );
}

#[test]
fn local_overload_prefers_candidate_with_fewer_omitted_defaults() {
    const SRC: &str = "fun box(): String {\n\
        \x20 fun choose(value: Int): String = \"OK\"\n\
        \x20 fun choose(value: Int, extra: Int = 0): String = \"wrong\"\n\
        \x20 return choose(1)\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("default overload tie-break"),
        "OK"
    );
}

#[test]
fn local_overload_prefers_non_vararg_candidate() {
    const SRC: &str = "fun box(): String {\n\
        \x20 fun choose(value: Int): String = \"OK\"\n\
        \x20 fun choose(value: Int, vararg rest: Int): String = \"wrong\"\n\
        \x20 return choose(1)\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("vararg overload tie-break"),
        "OK"
    );
}
