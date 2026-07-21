//! Resolution against a KRUSTY-compiled dependency DIRECTORY on the classpath — the separate-
//! compilation shape a multi-module build produces. The classpath layer must discover the dep's
//! file facades from the classes' own `@Metadata` (a class dir carries no `META-INF/*.kotlin_module`
//! unless the build writes one), so cross-module top-level functions AND extensions resolve exactly
//! as they do from a jar.

use super::common::{compile_and_run_box, compile_to_dir, jdk_modules, stdlib_jar};

/// Compile `lib` (krusty, in-process) into a fresh temp dir and return it. `None` ⇒ toolchain
/// absent ⇒ caller skips.
fn dep_dir(tag: &str, lib: &str) -> Option<std::path::PathBuf> {
    let stdlib = stdlib_jar()?;
    let dir = std::env::temp_dir().join(format!("krusty_depdir_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    compile_to_dir(lib, "lib1", &[stdlib], jdk_modules().as_deref(), &dir)
        .expect("dep lib must compile");
    Some(dir)
}

#[test]
fn top_level_fn_from_dep_dir() {
    let Some(dir) = dep_dir("toplevel", "package dep\n\nfun greet(): String = \"OK\"\n") else {
        return;
    };
    let Some(stdlib) = stdlib_jar() else { return };
    let main = "import dep.greet\n\nfun box(): String = greet()\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], jdk_modules().as_deref())
        .expect("top-level fn from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn extension_fn_from_dep_dir() {
    let Some(dir) = dep_dir(
        "ext",
        "package dep\n\nfun String.shout(): String = this + \"!\"\n",
    ) else {
        return;
    };
    let Some(stdlib) = stdlib_jar() else { return };
    let main = "import dep.shout\n\nfun box(): String {\n    return if (\"OK\".shout() == \"OK!\") \"OK\" else \"fail\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], jdk_modules().as_deref())
        .expect("extension fn from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}

// Two extension properties SHARING a name on different receivers: each metadata record must carry
// its own receiver (a name-only match would stamp one receiver on both).
#[test]
fn same_name_extension_properties_from_dep_dir() {
    let Some(dir) = dep_dir(
        "extprop2",
        "package dep\n\nval String.tagged: String\n    get() = \"s:\" + this\nval Int.tagged: String\n    get() = \"i:\" + this\n",
    ) else {
        return;
    };
    let Some(stdlib) = stdlib_jar() else { return };
    let main = "import dep.tagged\n\nfun box(): String {\n    if (\"x\".tagged != \"s:x\") return \"fail string\"\n    if (7.tagged != \"i:7\") return \"fail int\"\n    return \"OK\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], jdk_modules().as_deref())
        .expect("same-name extension properties from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn extension_property_from_dep_dir() {
    let Some(dir) = dep_dir(
        "extprop",
        "package dep\n\nval String.doubled: String\n    get() = this + this\n",
    ) else {
        return;
    };
    let Some(stdlib) = stdlib_jar() else { return };
    let main = "import dep.doubled\n\nfun box(): String {\n    return if (\"ab\".doubled == \"abab\") \"OK\" else \"fail\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], jdk_modules().as_deref())
        .expect("extension property from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}
