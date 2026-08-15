//! A `@JvmStatic` PROPERTY of a classpath `object` (`Dispatchers.IO`, `Dispatchers.Default`) read as
//! `Obj.prop`. In the language these are ordinary member properties; `@JvmStatic` only changes the JVM
//! realization — kotlinc emits the accessor as a static of the object class, so it landed in
//! `LibraryType::companion` and never in `members`, and the member lookup reported `unresolved reference
//! '<prop>'` for the whole `Dispatchers` surface. The JVM library provider now surfaces an accessor that
//! `@Metadata` declares as a member property in `members` too, and the JVM emitter (the only layer that
//! knows what `@JvmStatic` means) drops the receiver and emits `invokestatic`.
use super::common;

const LIB: &str = "package lib\n\
     object Cfg {\n\
     \x20 @JvmStatic val backed: String = \"B\"\n\
     \x20 @JvmStatic val computed: String get() = \"C\"\n\
     \x20 @JvmStatic var mutable: String = \"M\"\n\
     \x20 val instance: String = \"I\"\n\
     }\n";

#[test]
fn jvmstatic_object_property_read() {
    // Backed, computed, and `var` forms — all three accessors are JVM statics on `Cfg`.
    let main = "import lib.Cfg\n\
        fun box(): String {\n\
        \x20 if (Cfg.backed != \"B\") return \"fail backed: ${Cfg.backed}\"\n\
        \x20 if (Cfg.computed != \"C\") return \"fail computed: ${Cfg.computed}\"\n\
        \x20 if (Cfg.mutable != \"M\") return \"fail mutable: ${Cfg.mutable}\"\n\
        \x20 if (Cfg.instance != \"I\") return \"fail instance: ${Cfg.instance}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("jvmstatic_obj_prop", LIB, main);
}

/// The reported shape, against the real coroutines runtime: every `Dispatchers` member is an
/// `@JvmStatic val`, so `withContext(Dispatchers.IO)` reported `unresolved reference 'IO'`.
#[test]
fn coroutines_dispatchers_members_resolve() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    const SRC: &str = "import kotlinx.coroutines.Dispatchers\n\
        fun box(): String {\n\
        \x20 val io = Dispatchers.IO\n\
        \x20 if (io !== Dispatchers.IO) return \"fail: IO not stable\"\n\
        \x20 if (io === Dispatchers.Default) return \"fail: IO is Default\"\n\
        \x20 if (Dispatchers.Unconfined === Dispatchers.Default) return \"fail: Unconfined is Default\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classpath = [stdlib, coroutines, jdk.clone()];
    let out = common::compile_and_run_box(SRC, "Main", &classpath, Some(jdk.as_path()))
        .expect("Dispatchers members should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn jvmstatic_object_property_write_then_read() {
    // The write emitted `invokevirtual` on the singleton before this — an `IncompatibleClassChangeError`
    // at run time, not a compile error, so only a running box catches it.
    let main = "import lib.Cfg\n\
        fun box(): String {\n\
        \x20 Cfg.mutable = \"Z\"\n\
        \x20 return if (Cfg.mutable == \"Z\") \"OK\" else \"fail: ${Cfg.mutable}\"\n\
        }\n";
    common::expect_box_ok_against("jvmstatic_obj_prop_write", LIB, main);
}

/// The receiver carries no value for a static accessor, but it is still an expression: a bare
/// singleton/local read is elided (kotlinc emits neither), while a receiver that can have an EFFECT is
/// evaluated and popped. Both reads and writes go through the same emitter path.
#[test]
fn jvmstatic_object_property_receiver_effect_is_preserved() {
    let main = "import lib.Cfg\n\
        var trace = \"\"\n\
        fun side(): Cfg { trace += \"!\"; return Cfg }\n\
        fun box(): String {\n\
        \x20 val local = Cfg\n\
        \x20 if (local.backed != \"B\") return \"fail local: ${local.backed}\"\n\
        \x20 if (trace != \"\") return \"fail: local read had an effect\"\n\
        \x20 if (side().backed != \"B\") return \"fail effect-read\"\n\
        \x20 side().mutable = \"Z\"\n\
        \x20 if (Cfg.mutable != \"Z\") return \"fail effect-write: ${Cfg.mutable}\"\n\
        \x20 return if (trace == \"!!\") \"OK\" else \"fail trace: $trace\"\n\
        }\n";
    common::expect_box_ok_against("jvmstatic_obj_prop_receiver", LIB, main);
}
