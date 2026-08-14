//! Nullable extension receivers decoded from Kotlin metadata.

use super::common;

#[test]
fn kotlinc_compiled_nullable_and_unbounded_generic_extensions_accept_nullable_values() {
    let stdlib_path = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let libout = common::compile_lib(
        "nullable_extension_receiver",
        "package fixture\n\
         fun String?.nullableLength(): Int = this?.length ?: -1\n\
         fun <T> T.isNullGeneric(): Boolean = this == null\n",
    )
    .expect("scratch filesystem unavailable");

    let source = "import fixture.nullableLength\n\
                  import fixture.isNullGeneric\n\
                  fun box(): String {\n\
                  \u{20}\u{20}val missing: String? = null\n\
                  \u{20}\u{20}val present: String? = \"OK\"\n\
                  \u{20}\u{20}return if (missing.nullableLength() == -1 &&\n\
                  \u{20}\u{20}\u{20}\u{20}present.nullableLength() == 2 &&\n\
                  \u{20}\u{20}\u{20}\u{20}missing.isNullGeneric() && !present.isNullGeneric()) \"OK\" else \"fail\"\n\
                  }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(source, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile kotlinc-metadata nullable extensions");

    let output = common::run_box(&classes, "MainKt", &[libout, stdlib_path])
        .expect("pooled box runner unavailable");
    assert_eq!(output.trim(), "OK", "box() returned {output:?}");
}

#[test]
fn kotlinc_compiled_explicit_any_bound_rejects_nullable_receiver() {
    // The `T : Any` bound must reject a nullable receiver — checked in-process (the same checker
    // the CLI runs), no krusty CLI spawn.
    let diagnostics = common::diagnostics_against_ref(
        "non_null_generic_extension_receiver",
        "package fixture\nfun <T : Any> T.nonNullGeneric(): Int = 1\n",
        "import fixture.nonNullGeneric\nfun invalid(value: String?): Int = value.nonNullGeneric()\n",
    )
    .expect("provisioned kotlinc unavailable — run `just kotlinc \"$(just max-version)\"`");
    assert!(
        diagnostics.iter().any(|d| d.contains(
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        )),
        "unexpected diagnostics: {diagnostics:?}"
    );
}
