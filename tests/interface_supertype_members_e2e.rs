//! Member resolution against classpath `@Metadata`: members inherited through interface supertypes,
//! function-typed parameter members binding a lambda, `suspend` interface members (resolved + lowered to
//! a working coroutine call), a `@JvmStatic` object member, and a concrete generic return keeping its
//! type argument. Each dependency is built by the real kotlinc, so its metadata/bytecode is authoritative.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod common;

/// Build `src` with the real kotlinc into a fresh dir under `work`, returning it for the `-classpath`.
/// `None` (→ skip) when the kotlinc toolchain isn't provisioned.
fn build_lib(work: &std::path::Path, src: &str) -> Option<PathBuf> {
    let out = work.join("libout");
    fs::create_dir_all(&out).ok()?;
    let stdlib = common::stdlib_jar()?;
    let lib_kt = work.join("Lib.kt");
    fs::write(&lib_kt, src).ok()?;
    let args = vec![
        "-d".into(),
        out.to_string_lossy().into_owned(),
        "-cp".into(),
        format!("{}:{}", stdlib.display(), out.display()),
        lib_kt.to_string_lossy().into_owned(),
    ];
    matches!(common::kotlinc_compile(&args), Some((0, _))).then_some(out)
}

fn work_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("krusty_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Compile `main` against the kotlinc-built `lib` and run its `box()` on the JVM.
fn run_box_against(lib: &str, main: &str, tag: &str) -> Option<String> {
    let work = work_dir(tag);
    let libout = build_lib(&work, lib)?;
    let stdlib = common::stdlib_jar()?;
    let out = common::compile_and_run_box(
        main,
        "Main",
        &[libout, stdlib],
        common::jdk_modules().as_deref(),
    );
    let _ = fs::remove_dir_all(&work);
    out
}

#[test]
fn inherited_interface_members_and_lambda_param() {
    let lib = "package app\n\
        class Config(val name: String)\n\
        interface Named { val id: String }\n\
        interface CrudRepo {\n\
        \x20 fun save(c: Config): Config\n\
        \x20 fun findById(id: String): Config?\n\
        }\n\
        interface ConfigRepo : CrudRepo, Named\n\
        interface Logger { fun info(msg: () -> Any?) }\n";
    let main = "package app\n\
        class MemRepo(override val id: String) : ConfigRepo {\n\
        \x20 override fun save(c: Config) = c\n\
        \x20 override fun findById(id: String): Config? = Config(id)\n\
        }\n\
        fun use(r: ConfigRepo, log: Logger): String {\n\
        \x20 val s = r.save(Config(\"k\")); val f = r.findById(\"k\"); log.info { r.id }\n\
        \x20 return s.name + (f?.name ?: \"?\") + r.id\n\
        }\n\
        fun box() = if (use(MemRepo(\"R\"), object : Logger { override fun info(msg: () -> Any?) {} }) == \"kkR\") \"OK\" else \"fail\"\n";
    let Some(out) = run_box_against(lib, main, "iface_mem") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn concrete_generic_return_keeps_type_argument() {
    let lib = "package app\n\
        class Item(val id: String)\n\
        class Repo { fun all(): List<Item> = listOf(Item(\"1\")) }\n";
    let main = "package app\n\
        fun box(): String {\n\
        \x20 val r = Repo(); var s = \"\"\n\
        \x20 r.all().forEach { s += it.id }\n\
        \x20 return if (s == \"1\" && r.all().first().id == \"1\" && r.all()[0].id == \"1\") \"OK\" else \"fail\"\n\
        }\n";
    let Some(out) = run_box_against(lib, main, "gen_ret") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn named_args_to_classpath_constructor() {
    // NAMED arguments (out of order) to a CLASSPATH class constructor — krusty reads the ctor's parameter
    // names from `@Metadata` (`Constructor.value_parameter.name`) and reorders onto positions.
    let lib = "package app\n\
        class Point(val x: Int, val y: Int, val label: String)\n\
        data class Cfg(val host: String, val port: Int)\n";
    let main = "package app\n\
        fun box(): String {\n\
        \x20 val p = Point(y = 2, label = \"a\", x = 1)\n\
        \x20 val c = Cfg(port = 80, host = \"h\")\n\
        \x20 return if (p.x == 1 && p.y == 2 && p.label == \"a\" && c.host == \"h\" && c.port == 80) \"OK\" else \"fail\"\n\
        }\n";
    let Some(out) = run_box_against(lib, main, "named_ctor") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn jvmstatic_object_member() {
    let lib = "package app\n\
        object Ids { @JvmStatic fun of(s: String): String = s }\n";
    let main = "package app\n\
        fun box() = if (Ids.of(\"x\") == \"x\") \"OK\" else \"fail\"\n";
    let Some(out) = run_box_against(lib, main, "jvmstatic") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn suspend_interface_member() {
    // A `suspend` member is driven from Java with a completion `Continuation` (a suspend fn can't be
    // `box()`), the same shape `suspend_e2e.rs` uses.
    let Some(jh) = common::java_home() else {
        return;
    };
    if !std::path::Path::new(&format!("{jh}/bin/javac")).exists() {
        return;
    }
    let lib = "package app\n\
        class Config(val name: String)\n\
        interface CrudRepo { suspend fun getConfig(id: String): Config }\n\
        interface ConfigRepo : CrudRepo\n\
        class MemRepo : ConfigRepo { override suspend fun getConfig(id: String) = Config(id) }\n";
    let work = work_dir("susp_iface");
    let Some(libout) = build_lib(&work, lib) else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };

    fs::write(
        work.join("Main.kt"),
        "package app\n\
         suspend fun grab(r: ConfigRepo): String {\n\
         \x20 val c = r.getConfig(\"k\")\n\
         \x20 return c.name\n\
         }\n",
    )
    .unwrap();
    let cp = format!("{}:{}", libout.display(), stdlib.display());
    let ok = Command::new(env!("CARGO_BIN_EXE_krusty"))
        .args(["-cp", &cp, "-d", work.to_str().unwrap()])
        .arg(work.join("Main.kt"))
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "krusty failed on the suspend interface-member caller");

    fs::write(
        work.join("M.java"),
        "import kotlin.coroutines.*; import app.*;\n\
         public class M { public static void main(String[] a) {\n\
         Continuation<Object> k = new Continuation<Object>() {\n\
         public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
         public void resumeWith(Object o) {} };\n\
         System.out.println(\"k\".equals(MainKt.grab(new MemRepo(), k)) ? \"OK\" : \"fail\"); } }\n",
    )
    .unwrap();
    let cp = format!(
        "{}:{}:{}",
        work.to_str().unwrap(),
        libout.display(),
        stdlib.display()
    );
    let out = common::javac_run(
        work.join("M.java").to_str().unwrap(),
        &cp,
        work.to_str().unwrap(),
        "M",
    );
    let _ = fs::remove_dir_all(&work);
    if let Some(out) = out {
        assert_eq!(out.trim(), "OK");
    }
}
