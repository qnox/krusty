//! A property with a visibility-only setter (`var x = 0; private set`) is a plain backing-field
//! property with no setter method, as in kotlinc. Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn private_setter() {
    const SRC: &str = "class P {\n\
    var x: Int = 0\n\
        private set\n\
    fun bump() { this.x = this.x + 1 }\n\
}\n\
fun box(): String { val p = P(); p.bump(); p.bump(); return if (p.x == 2) \"OK\" else \"fail\" }\n";
    assert_eq!(run(SRC).expect("private set compiles + runs"), "OK");
}

#[test]
fn private_setter_is_not_writable_outside_its_owner() {
    // Moving default-accessor synthesis into a backend must not erase declaration visibility. This is a
    // frontend assertion as well as an ABI invariant: accepting the assignment would make the backend
    // synthesize a public setter for source that explicitly declared it private.
    let diagnostics = common::front_end_diagnostics(
        "class P { var x: Int = 0; private set }\n\
         fun misuse(p: P) { p.x = 1 }\n",
        &[],
        None,
    );
    assert_eq!(diagnostics, ["cannot access 'x': it is private in 'P'"]);
}

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

/// A default `private set` realizes no setter method, and the property's `@Metadata` names none.
/// The declaring class writes the backing field; ordinary and suspend lambdas, an inner class and
/// the companion write it through `access$set<X>$p`, while a read from them still calls the public
/// getter. An exported internal inline template uses the same bridge instead of naming the private
/// field or a nonexistent setter.
#[test]
fn a_default_private_setter_has_no_method_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Counter.kt",
        "package setters\n\
         \n\
         class Counter {\n\
         \x20   var count: Int = 1\n\
         \x20       private set\n\
         \x20   var label: String = \"\"\n\
         \x20       private set\n\
         \x20   fun bump() { count = count + 1 }\n\
         \x20   internal inline fun publishedWrite(value: Int) { count = value; label = \"inline\" }\n\
         \x20   fun later(): () -> Unit = { count = 5; label = \"later\" }\n\
         \x20   fun suspended(): suspend () -> Unit = { count = 6; label = \"suspended\" }\n\
         \x20   inner class Inner { fun reset() { count = count * 0 } }\n\
         \x20   companion object { fun reset(counter: Counter) { counter.count = counter.count - 1 } }\n\
         }\n",
    )]);
}

#[test]
fn an_override_and_a_hoisted_companion_private_default_setter_write_storage() {
    const SRC: &str = "interface A { val foo: String }\n\
class B : A {\n\
    override var foo: String = \"fail\"\n\
        private set\n\
    fun write(other: B) { other.foo = \"O\" }\n\
}\n\
class Outer {\n\
    class Nested { fun write(): String { s = \"K\"; return s } }\n\
    companion object {\n\
        var s = \"fail\"\n\
            private set\n\
    }\n\
}\n\
fun box(): String { val b = B(); b.write(b); return b.foo + Outer.Nested().write() }\n";
    assert_eq!(
        run(SRC).expect("private default setters compile + run"),
        "OK"
    );
}

/// A source-written or delegated private setter is not a default accessor, so each remains an
/// actual private method. This guards the semantic declaration predicate from using the presence of
/// a lowered function, which a backend may change while realizing the class.
#[test]
fn declared_private_setters_remain_methods() {
    const SRC: &str = "package setters\n\
import kotlin.reflect.KProperty\n\
class Delegate {\n\
    private var stored = 0\n\
    operator fun getValue(thisRef: Controlled, property: KProperty<*>): Int = stored\n\
    operator fun setValue(thisRef: Controlled, property: KProperty<*>, value: Int) { stored = value }\n\
}\n\
class Controlled {\n\
    var custom = 0\n\
        private set(value) { field = value }\n\
    var delegated: Int by Delegate()\n\
        private set\n\
    fun touch() { custom = 1; delegated = 2 }\n\
}\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(SRC, "Controlled", &[stdlib], Some(jdk.as_path()))
        .expect("declared private setters compile");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "setters/Controlled")
        .expect("setters/Controlled.class");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse Controlled.class");
    for name in ["setCustom", "setDelegated"] {
        let setter = class
            .method(name, "(I)V")
            .unwrap_or_else(|| panic!("{name}(I)V"));
        assert_eq!(setter.access & 0x0007, 0x0002, "{name} visibility");
    }
}

/// A `companion { … }` block property is a static of its class, with the same accessor rules as a
/// member: a `private set` produces no `setP` (and no setter signature in the class's
/// `@Metadata`), and each default accessor is mangled by its own visibility — an `internal set` on
/// a public property is `setW$<module>` beside `getW`, an internal property's are `getV$<module>`
/// and `setV$<module>` — which the class's metadata records and the facade's calls name.
#[test]
fn companion_block_private_and_internal_setters_match_kotlinc() {
    let src = "package store\n\
               \n\
               class C {\n\
               \x20   companion {\n\
               \x20       var w: String = \"\"; internal set\n\
               \x20       var p: String = \"\"; private set\n\
               \x20       internal var u: String = \"\"; private set\n\
               \x20       internal var v: String = \"\"\n\
               \x20       @PublishedApi internal var published: String = \"\"\n\
               \x20       fun touch() { w = \"O\"; p = \"y\"; u = \"x\" }\n\
               \x20   }\n\
               }\n\
               \n\
               fun box(): String {\n\
               \x20   C.touch()\n\
               \x20   C.w = \"O\"\n\
               \x20   C.v = \"K\"\n\
               \x20   C.published = \"!\"\n\
               \x20   return C.w + C.v + C.p + C.u + C.published\n\
               }\n";
    let settings = krusty::language_settings::LanguageSettings::new(
        krusty::language_version::LanguageVersion::V2_4,
        None,
        &[krusty::features::FeatureSetting {
            feature: "CompanionBlocksAndExtensions".to_string(),
            enabled: true,
        }],
    )
    .expect("Kotlin 2.4 with companion blocks");
    for class in ["store/C", "store/CompanionBlockSettersKt"] {
        let comparison = common::compare_with_kotlinc_plugin_language_settings(
            "CompanionBlockSetters",
            src,
            class,
            &[common::stdlib_jar()],
            "1.8",
            &settings,
        )
        .expect("reference kotlinc is provisioned");
        assert!(
            comparison.krusty_bytes == comparison.reference_bytes,
            "{class} differs from kotlinc's:\n{}\n---\n{}",
            comparison.krusty,
            comparison.reference
        );
    }
}

/// A sibling file reaches a `companion { … }` block member through the declaring class, by the
/// member's mangled JVM name: an `internal set`, an internal property's accessors and an internal
/// function and same-/sibling-file property-reference carriers all carry the module suffix there
/// too. A `@PublishedApi` property retains its public-ABI spelling.
#[test]
fn companion_block_internal_accessors_link_from_another_file() {
    const DECLARING: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        package store\n\
        class A {\n\
        \x20   companion {\n\
        \x20       var w: String = \"\"; internal set\n\
        \x20       internal var v: String = \"\"\n\
        \x20       @PublishedApi internal var published: String = \"\"\n\
        \x20       internal fun f(): String = \"!\"\n\
        \x20   }\n\
        }\n\
        internal fun sameFileReference(): String {\n\
        \x20   val ref = A::v\n\
        \x20   ref.set(\"K\")\n\
        \x20   return ref.get()\n\
        }\n";
    const USING: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        package store\n\
        fun box(): String {\n\
        \x20   val ref = A::w\n\
        \x20   ref.set(\"O\")\n\
        \x20   A.published = \"P\"\n\
        \x20   val r = ref.get() + sameFileReference() + A.f() + A.published\n\
        \x20   return if (r == \"OK!P\") \"OK\" else r\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("A.kt", DECLARING), ("Main.kt", USING)],
        "CompanionBlockInternalAccessors",
    );
}
