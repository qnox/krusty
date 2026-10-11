//! A class overriding a friend module's `internal` members (`-Xfriend-paths`, as a test source set
//! compiles against its main output). The overrides inherit `internal`, so each gets this module's
//! suffix (`getV$main`), and every overridden slot keeps the dependency module's (`getV$lib`) as an
//! overridable bridge that calls it. The dependency is built by kotlinc under `-module-name lib`.

use super::common;

const LIB: &str = "abstract class Base {\n\
    \x20   internal open fun plainFun(): Int = 1\n\
    \x20   internal open val stored: Int = 2\n\
    \x20   internal open val computed: Int\n\
    \x20       get() = 3\n\
    \x20   internal open var variable: Int = 4\n\
    \x20       set(given) { field = given }\n\
    }\n";

const MAIN: &str = "class Impl : Base() {\n\
    \x20   override fun plainFun(): Int = 10 * super.plainFun()\n\
    \x20   override val stored: Int = 20\n\
    \x20   override val computed: Int\n\
    \x20       get() = 10 * super.computed\n\
    \x20   override var variable: Int\n\
    \x20       set(mine) { super.variable = mine + 1 }\n\
    \x20       get() = 10 * super.variable\n\
    }\n\
    fun readThroughBase(base: Base): Int = base.plainFun() + base.stored + base.computed + base.variable\n\
    fun box(): String {\n\
    \x20   val impl = Impl()\n\
    \x20   val base: Base = impl\n\
    \x20   base.variable = 5\n\
    \x20   val total = readThroughBase(impl) + impl.variable\n\
    \x20   return if (total == 10 + 20 + 30 + 60 + 60) \"OK\" else \"total $total\"\n\
    }\n";

fn kotlinc(out: &std::path::Path, source: &std::path::Path, extra: &[String]) {
    let mut arguments = vec!["-d".to_string(), out.to_string_lossy().into_owned()];
    arguments.extend_from_slice(extra);
    arguments.push(source.to_string_lossy().into_owned());
    let (code, stderr) =
        common::kotlinc_compile(&arguments).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
}

/// Each method's name, descriptor and access flags, in classfile order.
fn methods(bytes: &[u8]) -> Vec<(String, String, u16)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect()
}

#[test]
fn overrides_of_friend_internal_members_publish_both_module_slots_like_kotlinc() {
    let dir = common::scratch_dir().expect("scratch directory");
    let lib_source = dir.join("Lib.kt");
    let main_source = dir.join("Main.kt");
    std::fs::write(&lib_source, LIB).expect("write library source");
    std::fs::write(&main_source, MAIN).expect("write main source");
    let library = dir.join("lib");
    kotlinc(
        &library,
        &lib_source,
        &["-module-name".to_string(), "lib".to_string()],
    );
    let reference = dir.join("main");
    let library_path = library.to_string_lossy().into_owned();
    kotlinc(
        &reference,
        &main_source,
        &[
            "-module-name".to_string(),
            "main".to_string(),
            "-classpath".to_string(),
            library_path.clone(),
            format!("-Xfriend-paths={library_path}"),
        ],
    );

    let runtime_classpath = [common::stdlib_jar(), library.clone()];
    let classes = common::compile_in_process_with_friend_paths(
        MAIN,
        "Main",
        &runtime_classpath,
        std::slice::from_ref(&library),
        Some(common::jdk_modules().as_path()),
    )
    .expect("overrides of friend internal members compile");
    let (_, emitted) = classes
        .iter()
        .find(|(name, _)| name == "Impl")
        .expect("krusty emits Impl");
    let expected = std::fs::read(reference.join("Impl.class")).expect("kotlinc emits Impl");
    assert_eq!(methods(emitted), methods(&expected));

    let box_class = common::find_box_class(&classes).expect("box() class");
    assert_eq!(
        common::run_box(&classes, &box_class, &runtime_classpath).expect("pooled box runner"),
        "OK"
    );
}
