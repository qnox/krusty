//! Nullable extension receivers decoded from Kotlin metadata.

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn kotlinc_compiled_nullable_and_unbounded_generic_extensions_accept_nullable_values() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(libout) = common::compile_lib(
        "nullable_extension_receiver",
        "package fixture\n\
         fun String?.nullableLength(): Int = this?.length ?: -1\n\
         fun <T> T.isNullGeneric(): Boolean = this == null\n",
    ) else {
        return;
    };

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
    let classes = common::compile_in_process(source, "Main", &cp, Some(&jdk_modules))
        .expect("krusty failed to compile kotlinc-metadata nullable extensions");

    let Some(output) = common::run_box(&classes, "MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(output.trim(), "OK", "box() returned {output:?}");
}

#[test]
fn kotlinc_compiled_explicit_any_bound_rejects_nullable_receiver() {
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let Some(libout) = common::compile_lib(
        "non_null_generic_extension_receiver",
        "package fixture\nfun <T : Any> T.nonNullGeneric(): Int = 1\n",
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!(
        "krusty_non_null_generic_extension_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("Main.kt");
    std::fs::write(
        &source,
        "import fixture.nonNullGeneric\nfun invalid(value: String?): Int = value.nonNullGeneric()\n",
    )
    .unwrap();
    let classpath = std::env::join_paths([libout, stdlib_path]).unwrap();
    let output = std::process::Command::new(common::krusty_binary())
        .args(["-cp", classpath.to_str().unwrap(), "-d"])
        .arg(root.join("out"))
        .arg(&source)
        .output()
        .unwrap();
    let rendered = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        !output.status.success(),
        "nullable call unexpectedly compiled"
    );
    assert!(
        rendered.contains(
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        ),
        "unexpected diagnostic: {rendered}"
    );
}
