//! The declarations `NOTHING_TO_OVERRIDE` and the abstract-member diagnostics list, rendered as
//! kotlinc renders them: each on its own line, with flexible Java types (`String!`,
//! `Array<(out) String!>!`), `vararg`, `suspend`, receivers, contexts, folded type-parameter bounds
//! and `= ...` defaults. A potential override is the supertype's member substituted at the
//! supertype: a generic substitution carries no displayed default, while a direct declaration
//! shows only defaults written there. An unimplemented abstract member is the declaration as its
//! classifier declares it.

use super::common;

/// Require kotlinc's complete errors for `sources` to be `expected`, and krusty's to equal them.
fn assert_complete_errors(sources: &[(&str, &str)], expected: &[&str]) {
    let reference_args = common::language_directives::kotlinc_args(sources[0].1);
    assert_eq!(
        common::reference_error_blocks(sources, &reference_args),
        expected
    );
    common::assert_error_blocks_match_kotlinc(sources, &reference_args);
}

const KOTLIN_SUPERTYPES: &str = r#"interface Shapes<T> {
    fun u(c: Class<out T>)
    suspend fun s(block: suspend T.(Int) -> Unit): T
    fun String.e(n: Int?): T?
    fun v(vararg xs: T, flag: Boolean = false)
    fun <A : CharSequence, B : T?> w(a: A, b: B) where A : Comparable<A>
    fun f(g: (T?) -> Unit, h: (() -> Unit)?): List<Map.Entry<T, String>>
    fun over(x: Int)
    fun over(x: String)
}
interface Plain {
    fun d(x: Int = 1, y: String)
    fun Int.q(vararg z: String = arrayOf())
    context(s: String) fun c(x: Int)
    fun <T> g(t: T = TODO()): T
}
abstract class K : Shapes<Int>, Plain {
    override fun u(c: Int) {}
    override suspend fun s(block: Int): Int = 0
    override fun Int.e(n: Int?): Int? = null
    override fun v(xs: Int) {}
    override fun w(a: Int) {}
    override fun f(): Int = 0
    override fun over(x: Long) {}
    override fun d(x: String) {}
    override fun Int.q(z: Int) {}
    override fun c(x: Int) {}
    override fun <T> g(): T = TODO()
    override fun absent() {}
}
"#;

/// A generic supertype's candidates are substituted and lose their defaults; a non-generic
/// supertype's keep them. Overloads are listed in declaration order.
#[test]
fn potential_overrides_from_kotlin_supertypes_match_kotlinc() {
    assert_complete_errors(
        &[("Main.kt", KOTLIN_SUPERTYPES)],
        &[
            "Main.kt:18:5: 'u' overrides nothing. Potential signatures for overriding:",
            "| fun u(c: Class<out Int>): Unit",
            "Main.kt:19:5: 's' overrides nothing. Potential signatures for overriding:",
            "| suspend fun s(block: suspend Int.(Int) -> Unit): Int",
            "Main.kt:20:5: 'e' overrides nothing. Potential signatures for overriding:",
            "| fun String.e(n: Int?): Int?",
            "Main.kt:21:5: 'v' overrides nothing. Potential signatures for overriding:",
            "| fun v(vararg xs: Int, flag: Boolean): Unit",
            "Main.kt:22:5: 'w' overrides nothing. Potential signatures for overriding:",
            "| fun <A : CharSequence, Comparable<A>, B : Int?> w(a: A, b: B): Unit",
            "Main.kt:23:5: 'f' overrides nothing. Potential signatures for overriding:",
            "| fun f(g: (Int?) -> Unit, h: (() -> Unit)?): List<Map.Entry<Int, String>>",
            "Main.kt:24:5: 'over' overrides nothing. Potential signatures for overriding:",
            "| fun over(x: Int): Unit",
            "| fun over(x: String): Unit",
            "Main.kt:25:5: 'd' overrides nothing. Potential signatures for overriding:",
            "| fun d(x: Int = ..., y: String): Unit",
            "Main.kt:26:5: 'q' overrides nothing. Potential signatures for overriding:",
            "| fun Int.q(vararg z: String = ...): Unit",
            "Main.kt:27:5: 'c' overrides nothing. Potential signatures for overriding:",
            "| context(s: String) fun c(x: Int): Unit",
            "Main.kt:28:5: 'g' overrides nothing. Potential signatures for overriding:",
            "| fun <T> g(t: T = ...): T",
            "Main.kt:29:5: 'absent' overrides nothing.",
        ],
    );
}

const INHERITED_CANDIDATES: &str = r#"interface Root {
    fun diamond(x: Int)
    fun inherited(x: Int = 1)
}
interface Left : Root
interface Right : Root
interface Middle : Root {
    override fun inherited(x: Int)
}
abstract class Diamond : Left, Right {
    override fun diamond(x: String) {}
}
abstract class DefaultImpl : Middle {
    override fun inherited(x: String) {}
}
"#;

/// A shared declaration reached through both sides of a diamond is listed once. Default argument
/// availability inherited by a non-generic override does not become syntax on that declaration.
#[test]
fn potential_overrides_keep_declaration_identity_and_default_provenance() {
    assert_complete_errors(
        &[("Main.kt", INHERITED_CANDIDATES)],
        &[
            "Main.kt:11:5: 'diamond' overrides nothing. Potential signatures for overriding:",
            "| fun diamond(x: Int): Unit",
            "Main.kt:14:5: 'inherited' overrides nothing. Potential signatures for overriding:",
            "| fun inherited(x: Int): Unit",
        ],
    );
}

const DECLARATION_RECEIVERS: &str = r#"interface Receivers {
    fun String?.nullable()
    fun (() -> Unit).function()
}
abstract class Derived : Receivers {
    override fun nullable(x: Int) {}
    override fun function(x: Int) {}
}
"#;

/// kotlinc's readable diagnostic renderer deliberately drops the source parentheses around a
/// function-type declaration receiver, while a nullable classifier receiver stays unparenthesized.
#[test]
fn declaration_receiver_parentheses_match_kotlinc() {
    assert_complete_errors(
        &[("Main.kt", DECLARATION_RECEIVERS)],
        &[
            "Main.kt:6:5: 'nullable' overrides nothing. Potential signatures for overriding:",
            "| fun String?.nullable(): Unit",
            "Main.kt:7:5: 'function' overrides nothing. Potential signatures for overriding:",
            "| fun () -> Unit.function(): Unit",
        ],
    );
}

const JAVA_SOURCE_BASE: &str = r#"public abstract class JShapes<T> {
    public abstract T value();
    public abstract String[] names();
    public abstract int[] counts();
    public abstract java.util.List<String> texts();
    public abstract java.util.Map<String, String> labels();
    public abstract <V> V any();
    public abstract <R extends Number> R number();
    public abstract void run();
}
"#;

const JAVA_SOURCE_SUBCLASSES: &str = r#"class F : JShapes<Int>()
abstract class M : JShapes<String>() {
    override fun value(x: Int): String = ""
    override fun names(x: Int) {}
    override fun any(x: Int) {}
    override fun number(x: Int) {}
}
"#;

/// A Java formal's implicit bound is the flexible `Any!`, which kotlinc shows.
#[test]
fn members_of_a_java_source_supertype_match_kotlinc() {
    assert_complete_errors(
        &[
            ("Main.kt", JAVA_SOURCE_SUBCLASSES),
            ("JShapes.java", JAVA_SOURCE_BASE),
        ],
        &[
            "Main.kt:1:1: class 'F' is not abstract and does not implement abstract base class members:",
            "| fun value(): T!",
            "| fun names(): Array<(out) String!>!",
            "| fun counts(): IntArray!",
            "| fun texts(): (Mutable)List<String!>!",
            "| fun labels(): (Mutable)Map<String!, String!>!",
            "| fun <V : Any!> any(): V!",
            "| fun <R : Number!> number(): R!",
            "| fun run(): Unit",
            "Main.kt:3:5: 'value' overrides nothing. Potential signatures for overriding:",
            "| fun value(): String!",
            "Main.kt:4:5: 'names' overrides nothing. Potential signatures for overriding:",
            "| fun names(): Array<(out) String!>!",
            "Main.kt:5:5: 'any' overrides nothing. Potential signatures for overriding:",
            "| fun <V : Any!> any(): V!",
            "Main.kt:6:5: 'number' overrides nothing. Potential signatures for overriding:",
            "| fun <R : Number!> number(): R!",
        ],
    );
}

const CLASSFILE_SUPERTYPES: &str = r#"class D : AbstractList<String>()
abstract class G : AbstractIterator<Long>() {
    override fun computeNext(x: Int) {}
}
abstract class L : java.util.concurrent.Callable<Long> {
    override fun call(x: Int): Long = 0
}
abstract class S : java.io.Writer() {
    override fun write(c: String, off: Int) {}
}
class E : java.util.AbstractList<String>(), Comparable<E>
class M : java.util.AbstractMap<String, Int>()
"#;

/// Kotlin metadata keeps parameter names; a JDK classfile's unnamed parameters are `p0`, `p1`, ….
/// A Java class declares a builtin property through its accessor (`AbstractCollection.size()`), and
/// the class chain claims a member before an interface at the same depth. A generic mapped accessor
/// is matched through its normalized override edge while the declaration renders unsubstituted.
#[test]
fn members_of_classfile_supertypes_match_kotlinc() {
    assert_complete_errors(
        &[("Main.kt", CLASSFILE_SUPERTYPES)],
        &[
            "Main.kt:1:1: class 'D' is not abstract and does not implement abstract base class members:",
            "| fun get(index: Int): E",
            "| val size: Int",
            "Main.kt:3:5: 'computeNext' overrides nothing. Potential signatures for overriding:",
            "| fun computeNext(): Unit",
            "Main.kt:6:5: 'call' overrides nothing. Potential signatures for overriding:",
            "| fun call(): Long!",
            "Main.kt:9:5: 'write' overrides nothing. Potential signatures for overriding:",
            "| fun write(p0: Int): Unit",
            "| fun write(p0: CharArray!): Unit",
            "| fun write(p0: CharArray!, p1: Int, p2: Int): Unit",
            "| fun write(p0: String!): Unit",
            "| fun write(p0: String!, p1: Int, p2: Int): Unit",
            "Main.kt:11:1: class 'E' is not abstract and does not implement abstract base class members:",
            "| fun get(p0: Int): E!",
            "| val size: Int",
            "Main.kt:11:1: class 'E' is not abstract and does not implement abstract member:",
            "| fun compareTo(other: T): Int",
            "Main.kt:12:1: class 'M' is not abstract and does not implement abstract base class member:",
            "| val entries: MutableSet<MutableMap.MutableEntry<K!, V!>>",
        ],
    );
}

/// A member extension remains a different Kotlin declaration kind even when its receiver happens
/// to occupy the same JVM parameter slot as an ordinary Java method. Frontend obligation checking
/// must not reinterpret the source declaration through a backend descriptor.
#[test]
fn a_member_extension_does_not_implement_an_ordinary_java_method() {
    let (library, _) = common::javac_compile(
        &[(
            "J.java".to_string(),
            "package fixture; public abstract class J { public abstract void f(J value); }"
                .to_string(),
        )],
        &[],
    )
    .expect("javac must build the ordinary-member fixture");
    let source = "class Derived : fixture.J() { fun fixture.J.f() {} }\n";
    let classpath = [library, common::stdlib_jar()];
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &classpath);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class member:",
            "| fun f(p0: J!): Unit",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// `fixture.Legacy`, compiled by kotlinc 1.7.22 with `-Xcontext-receivers`. Keeping the tiny class
/// inline makes the regression deterministic now that kotlinc 2.4 rejects legacy receiver source.
/// Its metadata carries typed legacy-receiver roles rather than named or anonymous parameters.
const LEGACY_CONTEXT_CLASS_HEX: &str = concat!(
    "cafebabe00000034002401000e666978747572652f4c65676163790700010100106a6176612f6c616e672f4f626a6563740700030100063c696e6974",
    "3e0100032829560c000500060a00040007010004746869730100104c666978747572652f4c65676163793b01000866756e6374696f6e010016284c6a",
    "6176612f6c616e672f537472696e673b4929560100234c6f72672f6a6574627261696e732f616e6e6f746174696f6e732f4e6f744e756c6c3b01000b",
    "67657450726f7065727479010015284c6a6176612f6c616e672f537472696e673b29490100114c6b6f746c696e2f4d657461646174613b0100026d76",
    "030000000103000000070100016b010002786903000000300100026431010081c080200a0218020a0210c0800a0208020a0210080a02100e0a020803",
    "0a0210020a020802082618c080320230014205c2a2060210024a1c10081a0230093206100a1a023004482652023005c3b801c080c2a20602100b5219",
    "10031a02300458c2a60462023005c3b801c080c2a206061a0408061007c28202070a0508c2b1143001010002643201000001000870726f7065727479",
    "010001780100064c69622e6b74010004436f646501000f4c696e654e756d6265725461626c650100124c6f63616c5661726961626c655461626c6501",
    "002452756e74696d65496e76697369626c65506172616d65746572416e6e6f746174696f6e7301000a536f7572636546696c6501001952756e74696d",
    "6556697369626c65416e6e6f746174696f6e730421000200040000000000030001000500060001001e0000002f00010001000000052ab70008b10000",
    "0002001f0000000600010000000200200000000c0001000000050009000a00000401000b000c0001002100000009020001000d000000000401000e00",
    "0f0001002100000007010001000d00000002002200000002001d00230000004f00010010000500115b00034900124900134900120014490012001549",
    "001600175b000173001800195b000c73000a73001a73000673001b73001a73001a73000e73000f73000b73001a73001c73000c",
);

fn decode_hex(hex: &str) -> Vec<u8> {
    fn nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("non-hex fixture byte"),
        }
    }
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty(), "hex fixture has an odd byte count");
    pairs
        .iter()
        .map(|[high, low]| nibble(*high) << 4 | nibble(*low))
        .collect()
}

/// A classpath declaration keeps a legacy context receiver distinct from both `context(name: T)`
/// and `context(_: T)`. The current reference renderer spells that role `<unused var>@T`.
#[test]
fn legacy_context_receivers_from_classpath_match_kotlinc() {
    let classpath = common::scratch_dir().expect("legacy context classpath");
    let class = classpath.join("fixture/Legacy.class");
    std::fs::create_dir_all(class.parent().expect("class package")).expect("create class package");
    std::fs::write(&class, decode_hex(LEGACY_CONTEXT_CLASS_HEX)).expect("write legacy class");
    let source = "import fixture.Legacy\nclass Derived : Legacy()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], std::slice::from_ref(&classpath));
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:2:1: class 'Derived' is not abstract and does not implement abstract base class members:",
            "| context(<unused var>@String) fun function(x: Int): Unit",
            "| context(<unused var>@String) val property: Int",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
    let _ = std::fs::remove_dir_all(classpath);
}

/// A generic extension property's bounds are declaration-owned metadata on its accessor signature;
/// rendering the abstract obligation must retain them rather than reducing each formal to a name.
#[test]
fn generic_abstract_property_bounds_from_classpath_match_kotlinc() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\nabstract class Base {\n    abstract val <T : CharSequence> T.current: T\n}\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class member:",
            "| val <T : CharSequence> T.current: T",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// The current-module provider publishes context identities and the same normalized accessor
/// signature as a dependency provider, so rendering needs no origin-specific recovery path.
#[test]
fn abstract_property_contexts_and_generic_bounds_from_module_symbols_match_kotlinc() {
    assert_complete_errors(
        &[
            (
                "Base.kt",
                "abstract class Base {\n    context(label: String) abstract val ordinary: Int\n    context(s: String) abstract val <T> T.plain: T\n    abstract val <T> T.current: T where T : CharSequence, T : Comparable<T>\n}\n",
            ),
            ("Main.kt", "class Derived : Base()\n"),
        ],
        &[
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class members:",
            "| context(label: String) val ordinary: Int",
            "| context(s: String) val <T> T.plain: T",
            "| val <T : CharSequence, Comparable<T>> T.current: T",
        ],
    );
}

/// A member-extension receiver is part of an override slot. Same-named abstract declarations that
/// differ only by that receiver remain separate obligations for both functions and properties, and
/// a concrete declaration on another receiver does not discharge one.
#[test]
fn receiver_overloaded_abstract_members_are_all_reported() {
    assert_complete_errors(
        &[
            (
                "Base.kt",
                "abstract class Base {\n    abstract fun String.f()\n    abstract fun Int.f()\n    abstract fun String.only()\n    open fun Int.only() {}\n    abstract fun <T> T.generic()\n    abstract val String.x: Int\n    abstract val Int.x: Int\n}\n",
            ),
            (
                "Main.kt",
                "class Derived : Base() { override fun <U> U.generic() {} }\n",
            ),
        ],
        &[
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class members:",
            "| fun String.f(): Unit",
            "| fun Int.f(): Unit",
            "| fun String.only(): Unit",
            "| val String.x: Int",
            "| val Int.x: Int",
        ],
    );
}

/// Kotlin unsigned types have distinct semantic identities even when a JVM backend represents and
/// erases them like their signed storage types. Obligation grouping is source semantics, so it must
/// retain both overloads and both member-extension receivers from a compiled dependency.
#[test]
fn unsigned_abstract_overloads_from_classpath_are_all_reported() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\nabstract class Base {\n    abstract fun f(x: Int)\n    abstract fun f(x: UInt)\n    abstract val Int.x: Int\n    abstract val UInt.x: Int\n}\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class members:",
            "| fun f(x: Int): Unit",
            "| fun f(x: UInt): Unit",
            "| val Int.x: Int",
            "| val UInt.x: Int",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Declaration-owned bounds remain part of an abstract function's semantic input shape. Distinct
/// bounded extension slots on an emitted abstract base must both survive obligation grouping.
#[test]
fn generic_shape_abstract_overloads_from_classpath_are_all_reported() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface A\ninterface B\nabstract class Base {\n    abstract fun <T : A> T.bounded()\n    abstract fun <T : B> T.bounded()\n}\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class members:",
            "| fun <T : A> T.bounded(): Unit",
            "| fun <T : B> T.bounded(): Unit",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// In these diagnostics kotlinc renders metadata-retained inner-class arguments as one flattened
/// classifier application, with the inner argument before the captured outer argument.
#[test]
fn nested_generic_classifier_segments_match_kotlinc() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\nclass Outer<A> { inner class Inner<B> }\nabstract class Base { abstract fun deep(value: Outer<String>.Inner<Int>) }\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "import fixture.Outer\ninterface Shapes { fun nested(value: Outer<String>.Inner<Int>) }\nabstract class Bad : Shapes {\n    override fun nested(value: Int) {}\n}\nclass Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:4:5: 'nested' overrides nothing. Potential signatures for overriding:",
            "| fun nested(value: Outer.Inner<Int, String>): Unit",
            "Main.kt:6:1: class 'Derived' is not abstract and does not implement abstract base class member:",
            "| fun deep(value: Outer.Inner<Int, String>): Unit",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Flattening is a Kotlin diagnostic convention, not a classpath-origin distinction.
#[test]
fn source_nested_generic_classifier_segments_match_kotlinc() {
    assert_complete_errors(
        &[(
            "Main.kt",
            "class Outer<A> { inner class Inner<B> }\n\
             interface Shapes { fun nested(value: Outer<String>.Inner<Int>) }\n\
             abstract class Bad : Shapes {\n\
                 override fun nested(value: Int) {}\n\
             }\n",
        )],
        &[
            "Main.kt:4:1: 'nested' overrides nothing. Potential signatures for overriding:",
            "| fun nested(value: Outer.Inner<Int, String>): Unit",
        ],
    );
}

/// Upper bounds form a semantic set. Reordering the same bounds and alpha-renaming the formal in a
/// second supertype still contributes one abstract property obligation.
#[test]
fn reordered_generic_property_bounds_form_one_obligation() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface A\ninterface B\ninterface Left { val <T : A> T.x: Int where T : B }\ninterface Right { val <U : B> U.x: Int where U : A }\nabstract class Base : Left, Right\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val <T : A, B> T.x: Int",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// A covariant fake override reports the uniquely most-specific result rather than the first
/// same-input property contributed by the supertype traversal.
#[test]
fn covariant_abstract_property_reports_most_specific_result() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface Wide { val owner: Any }\ninterface Narrow { val owner: String }\nabstract class Base : Wide, Narrow\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val owner: String",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Recovering a covariant fake override must retain its most-specific result without attributing
/// the interface obligation to the abstract source class that merely inherits it.
#[test]
fn source_covariant_property_recovery_keeps_interface_provenance() {
    assert_complete_errors(
        &[
            (
                "Base.kt",
                "package fixture\n\ninterface Wide { val owner: Any }\ninterface Narrow { val owner: String }\nabstract class Base : Wide, Narrow\n",
            ),
            ("Main.kt", "class Derived : fixture.Base()\n"),
        ],
        &[
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val owner: String",
        ],
    );
}

/// An abstract source class does not become the diagnostic owner of every later interface
/// obligation merely because it precedes that interface in the hierarchy. Only an exact property
/// slot presented through that class's own hierarchy is a source fake override.
#[test]
fn unrelated_source_abstract_class_does_not_claim_interface_property() {
    assert_complete_errors(
        &[
            (
                "Base.kt",
                "package fixture\n\nabstract class Base { fun marker() = Unit }\ninterface Named { val owner: String }\n",
            ),
            ("Main.kt", "class Derived : fixture.Base(), fixture.Named\n"),
        ],
        &[
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val owner: String",
        ],
    );
}

/// Equal-result `val` and `var` declarations form one fake override, but the mutable declaration
/// owns the setter capability that the concrete subclass is also missing.
#[test]
fn mutable_abstract_property_wins_over_an_equally_specific_val() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface ReadOnly { val value: String }\ninterface Mutable { var value: String }\nabstract class Base : ReadOnly, Mutable\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| var value: String",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Alpha-renamed declaration variables describe one generic property slot, including its result;
/// result specificity must compare their semantic ordinals rather than the source names `T`/`U`.
#[test]
fn alpha_renamed_generic_property_results_form_one_obligation() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface Left { val <T> T.x: T }\ninterface Right { val <U> U.x: U }\nabstract class Base : Left, Right\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val <T> T.x: T",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Alpha-normalizing a generic result must retain its declared bound. The bound supplies the
/// subtype proof that selects `T` over `A`; replacing it with the implicit `Any?` would leave the
/// fake override without a uniquely most-specific result.
#[test]
fn bounded_generic_property_result_retains_subtype_proof() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface A\ninterface B : A\ninterface Left { val <T : B> T.x: T }\ninterface Right { val <U : B> U.x: A }\nabstract class Base : Left, Right\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val <T : B> T.x: T",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Additional `where` bounds live in the declaration's generic signature rather than the inline
/// `TyParam` bound. Covariant result selection must rebuild the complete bound intersection before
/// deciding that `T : A, B` is more specific than `B`.
#[test]
fn secondary_generic_property_bound_retains_subtype_proof() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface A\ninterface B\ninterface Left { val <T> T.x: T where T : A, T : B }\ninterface Right { val <U> U.x: B where U : A, U : B }\nabstract class Base : Left, Right\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val <T : A, B> T.x: T",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// A bound may reach another formal whose complete constraints include a secondary bound. Result
/// normalization must hydrate that chain so `T : U` and `U : A, B` proves `T : B`.
#[test]
fn transitive_generic_property_bound_retains_subtype_proof() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface A\ninterface B\ninterface Left { val <T, U> T.x: T where T : U, U : A, U : B }\ninterface Right { val <V, W> V.x: B where V : W, W : A, W : B }\nabstract class Base : Left, Right\n",
    )])
    .expect("reference kotlinc is provisioned");
    let source = "class Derived : fixture.Base()\n";
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], &[library, common::stdlib_jar()]);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(
        expected,
        [
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract member:",
            "| val <T : U, U : A, B> T.x: T",
        ]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}

/// Kotlin metadata omits an unbounded formal's implicit `Any?`, while the source provider may
/// publish that semantic default explicitly. Both representations belong to the same override
/// slot, so a concrete source declaration discharges the compiled interface obligation.
#[test]
fn implicit_generic_property_bound_matches_across_providers() {
    let library = common::kotlinc_lib_out(&[(
        "Lib.kt",
        "package fixture\n\ninterface Contract { val <T> T.value: Int }\n",
    )])
    .expect("reference kotlinc is provisioned");
    let sources = [
        (
            "Base.kt",
            "abstract class Base : fixture.Contract {\n    override val <T> T.value: Int get() = 1\n}\n",
        ),
        ("Main.kt", "class Derived : Base()\n"),
    ];
    let result = common::compiler_diagnostics(&sources, &[library, common::stdlib_jar()]);
    assert_eq!(
        common::rendered_error_blocks(&result.reference_stderr, true),
        [] as [&str; 0]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        [] as [&str; 0]
    );
}

/// A property formal may be bounded by its declaring classifier's formal. Applied hierarchy
/// specialization must substitute that owner parameter in the override-slot key while retaining
/// the declaration-relative signature for diagnostics.
#[test]
fn generic_property_bound_uses_the_applied_owner_argument() {
    let sources = [
        (
            "Base.kt",
            "interface B\ninterface Parent<X> { val <T : X> T.x: Int }\nabstract class Base : Parent<B> { override val <U : B> U.x: Int get() = 1 }\n",
        ),
        ("Main.kt", "class Derived : Base()\n"),
    ];
    let result = common::compiler_diagnostics(&sources, &[common::stdlib_jar()]);
    assert_eq!(
        common::rendered_error_blocks(&result.reference_stderr, true),
        [] as [&str; 0]
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        [] as [&str; 0]
    );
}

/// Potential overrides list every same-named declaration kotlinc offers, including both ordinary
/// and member-extension declarations and independently of extension-receiver applicability.
#[test]
fn potential_overrides_include_all_same_named_declarations() {
    assert_complete_errors(
        &[(
            "Main.kt",
            "interface Mixed {\n    fun String.f()\n    fun f(x: Int)\n}\nabstract class Derived : Mixed {\n    override fun f() {}\n    override fun Int.f(x: Int) {}\n}\n",
        )],
        &[
            "Main.kt:6:5: 'f' overrides nothing. Potential signatures for overriding:",
            "| fun String.f(): Unit",
            "| fun f(x: Int): Unit",
            "Main.kt:7:5: 'f' overrides nothing. Potential signatures for overriding:",
            "| fun String.f(): Unit",
            "| fun f(x: Int): Unit",
        ],
    );
}

/// A platform-flexible function type needs parentheses around the whole arrow type before `!`;
/// otherwise the marker appears to qualify only its result.
#[test]
fn java_platform_function_type_result_matches_kotlinc() {
    assert_complete_errors(
        &[
            ("Main.kt", "class Derived : JCallback()\n"),
            (
                "JCallback.java",
                "public abstract class JCallback {\n    public abstract kotlin.jvm.functions.Function1<String, Integer> callback();\n}\n",
            ),
        ],
        &[
            "Main.kt:1:1: class 'Derived' is not abstract and does not implement abstract base class member:",
            "| fun callback(): ((String!) -> Int!)!",
        ],
    );
}

const BODY_LOCAL_BASE: &str = r#"abstract class Source {
    abstract fun read(): Int
    abstract fun write(value: Int): String
}
"#;

const BODY_LOCAL_SUBCLASSES: &str = r#"fun anonymous(): Source = object : Source() {
    override fun read() = 1
}

fun local(): Source {
    class Partial : Source() {
        override fun write(value: Int) = "$value"
    }
    return Partial()
}

fun nested(): Any {
    class Outer {
        inner class Missing : Source() {
            override fun read() = 2
        }
    }
    return Outer()
}
"#;

/// A body-local class checks its obligations after its methods, so an override whose result its
/// body infers discharges its member while the other one is still reported. kotlinc names an
/// object expression `<anonymous>` and a local classifier from its outermost local class.
#[test]
fn body_local_classes_with_inferred_overrides_match_kotlinc() {
    assert_complete_errors(
        &[("Main.kt", BODY_LOCAL_SUBCLASSES), ("Base.kt", BODY_LOCAL_BASE)],
        &[
            "Main.kt:1:27: class '<anonymous>' is not abstract and does not implement abstract base class member:",
            "| fun write(value: Int): String",
            "Main.kt:6:5: class 'Partial' is not abstract and does not implement abstract base class member:",
            "| fun read(): Int",
            "Main.kt:14:15: class 'Outer.Missing' is not abstract and does not implement abstract base class member:",
            "| fun write(value: Int): String",
        ],
    );
}

const BODY_LOCAL_INFERRED_BASE: &str = r#"abstract class Source {
    abstract fun next(): Boolean
}

abstract class Adapter<T> {
    abstract fun read(source: Source): T
    abstract fun write(value: T): String
}
"#;

const BODY_LOCAL_INFERRED_OVERRIDES: &str = r#"class Constant(private val value: Boolean) : Source() {
    override fun next() = value
}

val booleans: Adapter<Boolean> = object : Adapter<Boolean>() {
    override fun read(source: Source) = source.next()
    override fun write(value: Boolean) = value.toString()
}

fun negated(): Adapter<Boolean> {
    class Negated : Adapter<Boolean>() {
        override fun read(source: Source) = !source.next()
        override fun write(value: Boolean) = "not $value"
    }
    return Negated()
}

fun box(): String {
    if (!booleans.read(Constant(true))) return "fail: anonymous read"
    if (booleans.write(false) != "false") return "fail: anonymous write"
    if (negated().read(Constant(true))) return "fail: local read"
    if (negated().write(true) != "not true") return "fail: local write"
    return "OK"
}
"#;

/// A local or anonymous class implements an abstract member of a superclass declared in another
/// file with an override whose result its expression body infers. The Wasm and Native KLIB lanes do
/// not lower a superclass declared outside the file yet, so the runtime result and the complete
/// class inventory are compared with kotlinc here.
#[test]
fn body_local_inferred_overrides_across_files_run_like_kotlinc() {
    let sources = [
        ("base.kt", BODY_LOCAL_INFERRED_BASE),
        ("main.kt", BODY_LOCAL_INFERRED_OVERRIDES),
    ];
    let reference_args = common::language_directives::kotlinc_args(sources[0].1);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        Vec::<String>::new()
    );
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[sources[0].1, sources[1].1]),
        Vec::<String>::new()
    );
    common::expect_box_ok_files_with_stdlib(&sources, "BodyLocalInferredOverrides");
    let differences = common::classes_against_kotlinc_module(&sources).differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
