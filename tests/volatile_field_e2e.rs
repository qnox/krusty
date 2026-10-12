//! `@Volatile` (`kotlin.concurrent.Volatile`, the JVM's typealias of `kotlin.jvm.Volatile`) makes
//! the property's backing field `ACC_VOLATILE`. kotlinc sets the flag on every storage shape the
//! property can take: a class instance field whatever its visibility, a `lateinit` field, a
//! companion property hoisted onto its outer class as a static, an object's static, and a file
//! facade's static. `AtomicReferenceFieldUpdater.newUpdater` refuses a field without the flag
//! ("Must be volatile type").
use super::common;

/// Every storage shape side by side. The private companion property is both read and written from
/// the companion so its `access$get…$cp`/`access$set…$cp` bridges exist on both sides.
const FIELDS: &str = "package store\n\
    \n\
    import kotlin.concurrent.Volatile\n\
    \n\
    class Holder(@Volatile var primary: Int, @Volatile private var secret: String) {\n\
    \x20   @Volatile private var s: String? = null\n\
    \x20   @kotlin.jvm.Volatile var pub: Int = 0\n\
    \x20   @Volatile lateinit var late: String\n\
    \x20   @Volatile internal var inter: Long = 1L\n\
    \x20   var plain: Int = 0\n\
    \n\
    \x20   fun current(): String? = s\n\
    \x20   fun reveal(): String = secret\n\
    \n\
    \x20   companion object {\n\
    \x20       @Volatile var comp: Int = 3\n\
    \x20       @Volatile private var hidden: String = \"h\"\n\
    \x20       fun swapHidden(value: String): String {\n\
    \x20           val old = hidden\n\
    \x20           hidden = value\n\
    \x20           return old\n\
    \x20       }\n\
    \x20   }\n\
    }\n\
    \n\
    object Registry {\n\
    \x20   @Volatile var counter: Int = 0\n\
    \x20   @Volatile lateinit var entry: String\n\
    }\n\
    \n\
    @Volatile var top: Int = 0\n\
    @Volatile private var topPrivate: String = \"t\"\n\
    @Volatile lateinit var topLate: String\n\
    \n\
    fun topPrivateValue(): String = topPrivate\n";

#[test]
fn volatile_fields_are_byte_identical_to_kotlinc() {
    for class in [
        "store/Holder",
        "store/Holder$Companion",
        "store/Registry",
        "store/VolatileFieldsKt",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "VolatileFields",
            FIELDS,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} byte-identical to kotlinc: {diff}"));
    }
}

/// The issue's shape: a field updater over a `@Volatile private var`, plus a reflective check that
/// every shape above (and only the annotated ones) carries the JVM modifier at run time.
const UPDATER: &str = "package store\n\
    \n\
    import java.lang.reflect.Modifier\n\
    import java.util.concurrent.atomic.AtomicReferenceFieldUpdater\n\
    import kotlin.concurrent.Volatile\n\
    \n\
    class Holder(@Volatile var primary: Int) {\n\
    \x20   @Volatile private var s: String? = null\n\
    \x20   @kotlin.jvm.Volatile var pub: Int = 0\n\
    \x20   @Volatile lateinit var late: String\n\
    \x20   var plain: Int = 0\n\
    \n\
    \x20   fun cas(expect: String?, update: String): Boolean = UPDATER.compareAndSet(this, expect, update)\n\
    \x20   fun current(): String? = s\n\
    \n\
    \x20   companion object {\n\
    \x20       @Volatile var comp: Int = 3\n\
    \x20       private val UPDATER =\n\
    \x20           AtomicReferenceFieldUpdater.newUpdater(Holder::class.java, String::class.java, \"s\")\n\
    \x20   }\n\
    }\n\
    \n\
    object Registry {\n\
    \x20   @Volatile var counter: Int = 0\n\
    }\n\
    \n\
    @Volatile var top: Int = 0\n\
    @Volatile lateinit var topLate: String\n\
    \n\
    fun isVolatile(owner: Class<*>, name: String): Boolean =\n\
    \x20   Modifier.isVolatile(owner.getDeclaredField(name).modifiers)\n\
    \n\
    fun box(): String {\n\
    \x20   val holder = Holder(1)\n\
    \x20   if (!holder.cas(null, \"a\")) return \"fail: first cas\"\n\
    \x20   if (holder.cas(null, \"b\")) return \"fail: second cas\"\n\
    \x20   if (holder.current() != \"a\") return \"fail: value\"\n\
    \x20   val facade = Class.forName(\"store.VolatileUpdaterKt\")\n\
    \x20   val holderClass = Holder::class.java\n\
    \x20   for (name in arrayOf(\"primary\", \"s\", \"pub\", \"late\", \"comp\")) {\n\
    \x20       if (!isVolatile(holderClass, name)) return \"fail: Holder.\" + name\n\
    \x20   }\n\
    \x20   if (isVolatile(holderClass, \"plain\")) return \"fail: plain\"\n\
    \x20   if (!isVolatile(Registry::class.java, \"counter\")) return \"fail: counter\"\n\
    \x20   if (!isVolatile(facade, \"top\")) return \"fail: top\"\n\
    \x20   if (!isVolatile(facade, \"topLate\")) return \"fail: topLate\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn volatile_field_updater_accepts_the_field() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(UPDATER, "VolatileUpdater", &[stdlib], Some(jdk.as_path())),
        "OK"
    );
}
