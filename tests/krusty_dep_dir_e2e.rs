//! Resolution against a KRUSTY-compiled dependency DIRECTORY on the classpath — the separate-
//! compilation shape a multi-module build produces. The classpath layer must discover the dep's
//! file facades from the classes' own `@Metadata` (a class dir carries no `META-INF/*.kotlin_module`
//! unless the build writes one), so cross-module top-level functions AND extensions resolve exactly
//! as they do from a jar.

use super::common::{
    compile_and_run_box, compile_to_dir, front_end_diagnostics, jdk_modules, stdlib_jar,
};

/// Compile `lib` (krusty, in-process) into a fresh temp dir and return it. `None` ⇒ toolchain
/// absent ⇒ caller skips.
fn dep_dir(tag: &str, lib: &str) -> Option<std::path::PathBuf> {
    let stdlib = stdlib_jar();
    let dir = std::env::temp_dir().join(format!("krusty_depdir_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    compile_to_dir(lib, "lib1", &[stdlib], Some(jdk_modules().as_path()), &dir)
        .expect("dep lib must compile");
    Some(dir)
}

#[test]
fn top_level_fn_from_dep_dir() {
    let Some(dir) = dep_dir("toplevel", "package dep\n\nfun greet(): String = \"OK\"\n") else {
        return;
    };
    let stdlib = stdlib_jar();
    let main = "import dep.greet\n\nfun box(): String = greet()\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
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
    let stdlib = stdlib_jar();
    let main = "import dep.shout\n\nfun box(): String {\n    return if (\"OK\".shout() == \"OK!\") \"OK\" else \"fail\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
        .expect("extension fn from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn typealias_from_dep_dir_is_a_classifier_declaration() {
    let Some(dir) = dep_dir(
        "typealias",
        "package dep\n\nclass Real(val value: String)\ntypealias Alias = Real\n",
    ) else {
        return;
    };
    let stdlib = stdlib_jar();
    let main = "import dep.Alias\n\nfun box(): String = Alias(\"OK\").value\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
        .expect("typealias from a krusty-built dependency must compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn fun_interface_metadata_survives_a_dependency_boundary() {
    let Some(dir) = dep_dir(
        "fun_interface",
        "package dep\n\n\
         interface Marker { val marker: Boolean get() = false }\n\
         fun interface Action : Marker { fun run() }\n",
    ) else {
        return;
    };
    let stdlib = stdlib_jar();
    let main = "import dep.Action\n\n\
                class Consumer { fun accept(action: Action) { action.run() } }\n\
                fun forward(consumer: Consumer, callback: () -> Unit) { consumer.accept(callback) }\n\
                fun box(): String { forward(Consumer()) {}; return \"OK\" }\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
        .expect("fun-interface metadata from a krusty-built dependency must enable SAM conversion");
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
    let stdlib = stdlib_jar();
    let main = "import dep.tagged\n\nfun box(): String {\n    if (\"x\".tagged != \"s:x\") return \"fail string\"\n    if (7.tagged != \"i:7\") return \"fail int\"\n    return \"OK\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
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
    let stdlib = stdlib_jar();
    let main = "import dep.doubled\n\nfun box(): String {\n    return if (\"ab\".doubled == \"abab\") \"OK\" else \"fail\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
        .expect("extension property from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn generic_mutable_extension_property_reference_from_dep_dir() {
    let Some(dir) = dep_dir(
        "generic_extprop_ref",
        "class C<T>(var value: T)\n\
         var <T> C<T>.live: T\n\
         \x20 get() = value\n\
         \x20 set(next) { value = next }\n",
    ) else {
        return;
    };
    let stdlib = stdlib_jar();
    let main = "import kotlin.reflect.KMutableProperty0\n\
        fun update(property: KMutableProperty0<String>): String {\n\
        \x20 property.set(\"OK\")\n\
        \x20 return property.get()\n\
        }\n\
        fun box(): String {\n\
        \x20 val c = C(\"fail\")\n\
        \x20 return update(c::live)\n\
        }\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
        .expect("generic mutable extension property reference from a krusty-built dep dir");
    assert_eq!(out, "OK");
}

/// The multi-module conformance shape (`inlineSizeReduction/multiModuleDefaultArgsCleanup`): an
/// inline fn with defaulted params in a KRUSTY-built dep, called with named/omitted arguments
/// from another module. The `$default` body must not be spliced and the slot mapping must hold.
#[test]
fn inline_fn_with_defaults_named_calls_from_dep_dir() {
    let Some(dir) = dep_dir(
        "inline_named",
        "package dep\n\ninline fun foo(x: String = \"x\", y: String = \"y\") = x + y\n",
    ) else {
        return;
    };
    let stdlib = stdlib_jar();
    let main = "import dep.foo\n\nfun box(): String {\n    val r = foo() + \";\" + foo(x = \"X\") + \";\" + foo(y = \"Y\") + \";\" + foo(x = \"X\", y = \"Y\")\n    return if (r == \"xy;Xy;xY;XY\") \"OK\" else \"fail: $r\"\n}\n";
    let out = compile_and_run_box(main, "Main", &[dir, stdlib], Some(jdk_modules().as_path()))
        .expect("inline fn with defaults from a krusty-built dep dir must compile and run");
    assert_eq!(out, "OK");
}

#[test]
fn legacy_inline_class_member_from_dep_dir_shapes_implicit_it() {
    let Some(dir) = dep_dir(
        "member_lambda",
        "// LANGUAGE: +InlineClasses\npackage dep\n\n\
         interface Text { fun text(): String }\n\
         inline class Wrapper(val text: String) : Text {\n\
           constructor(number: Int) : this(number.toString())\n\
           override fun text(): String = text\n\
           fun plain(): String = text\n\
           inline fun <T> run(transform: (String) -> T): T = transform(text)\n\
           companion object { fun make(number: Int) = Wrapper(number) }\n\
         }\n",
    ) else {
        return;
    };
    let stdlib = stdlib_jar();
    let main = "// LANGUAGE: +InlineClasses\nimport dep.Wrapper\n\n\
                fun read(wrapper: Wrapper): String = wrapper.run { it }\n\
                fun box(): String = \"OK\"\n";
    let classpath = [dir.clone(), stdlib.clone()];
    let diagnostics = front_end_diagnostics(main, &classpath, Some(jdk_modules().as_path()));
    assert!(
        diagnostics.is_empty(),
        "dependency member lambda diagnostics: {diagnostics:?}"
    );
}
