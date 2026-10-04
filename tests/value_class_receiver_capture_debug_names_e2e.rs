//! kotlinc lowers a value class's members to statics before it lifts the lambdas and local
//! functions written in them, so what such a callable captures as `this` is no longer a dispatch
//! receiver: it is whatever value the static realizes the receiver as, and the lifted parameter is
//! named after that value with kotlinc's `$` capture prefix. A member or accessor's `-impl` takes the
//! receiver as its carrier parameter `arg0` (`$arg0`); the primary `constructor-impl` running the
//! `init` blocks holds it in its first temporary `tmp0` (`$tmp0`); a secondary constructor's
//! `constructor-impl` names that temporary after `$this` (`$tmp0_$this`).
//!
//! krusty spelled every such capture like an ordinary class's enclosing instance, `this$0`.
use super::common;

const SOURCE: &str = "class Two(val first: Any, val second: Any)\n\
    @JvmInline value class Tag(val s: String)\n\
    @JvmInline value class V(val s: String) {\n\
    \x20   init {\n\
    \x20       val f: () -> Any = { this }\n\
    \x20       f()\n\
    \x20   }\n\
    \x20   constructor(t: Tag, u: Tag) : this(u.s) {\n\
    \x20       val g: () -> Any = { this }\n\
    \x20       g()\n\
    \x20   }\n\
    \x20   fun m(): () -> Any = { this }\n\
    \x20   val p: () -> Any get() = { this }\n\
    \x20   fun nested(): () -> () -> Any = { { this } }\n\
    \x20   fun local(): Any {\n\
    \x20       fun loc(): Any = this\n\
    \x20       return loc()\n\
    \x20   }\n\
    \x20   fun localInLambda(): () -> Any = {\n\
    \x20       fun loc(): Any = this\n\
    \x20       loc()\n\
    \x20   }\n\
    \x20   fun withTag(t: Tag): () -> Any = { Two(t, this) }\n\
    \x20   fun Tag.mext(): () -> Any = { Two(this@V, this) }\n\
    }\n\
    fun V.ext(): () -> Any = { this }\n";

/// The lifted methods kotlinc writes, by class, as `javap -p` declares them, in class-file order.
const LIFTED: &[(&str, &str)] = &[
    (
        "V",
        "private static final java.lang.Object local_impl$loc(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object localInLambda_impl$lambda$0$loc(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object constructor_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object m_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object getP_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object nested_impl$lambda$0$0(java.lang.String);",
    ),
    (
        "V",
        "private static final kotlin.jvm.functions.Function0 nested_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object localInLambda_impl$lambda$0(java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object withTag_txdesME$lambda$0(java.lang.String, java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object mext_txdesME$lambda$0(java.lang.String, java.lang.String);",
    ),
    (
        "V",
        "private static final java.lang.Object constructor_impl$lambda$1(java.lang.String);",
    ),
    (
        "CaptureKt",
        "private static final java.lang.Object ext_Wv3fJ18$lambda$0(java.lang.String);",
    ),
];

#[test]
fn a_value_class_receiver_capture_is_named_after_its_static_realization_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Capture.kt", SOURCE)]);
    for class in ["V", "CaptureKt"] {
        let (reference, krusty) = classes
            .method_declarations(class)
            .unwrap_or_else(|| panic!("both compilers write {class}"));
        let lifted = |declarations: &[String]| {
            declarations
                .iter()
                .filter(|declaration| declaration.starts_with("private static final"))
                .filter(|declaration| declaration.contains('$'))
                .cloned()
                .collect::<Vec<_>>()
        };
        let expected = LIFTED
            .iter()
            .filter(|(owner, _)| *owner == class)
            .map(|(_, declaration)| declaration.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            lifted(&reference),
            expected,
            "kotlinc's lifted methods of {class}"
        );
        // krusty places a value class's lifted methods ahead of its synthesized members; kotlinc
        // writes them last. That order is apart from their names and code.
        let mut krusty = lifted(&krusty);
        let mut expected = expected;
        krusty.sort();
        expected.sort();
        assert_eq!(krusty, expected, "krusty's lifted methods of {class}");
    }
    // Each lifted method matches whole: its code, line numbers and local-variable table, where the
    // captured receiver is `$arg0`, `$tmp0` or `$tmp0_$this` after its value-class realization.
    // A bootstrap-method index is the class's own layout, apart from any one method's code.
    let bootstrap = |listing: String| {
        listing
            .split("InvokeDynamic #")
            .enumerate()
            .map(|(index, part)| match index {
                0 => part.to_string(),
                _ => part
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .to_string(),
            })
            .collect::<Vec<_>>()
            .join("InvokeDynamic #")
    };
    for (class, declaration) in LIFTED {
        let (reference, krusty) = classes.method_listing(class, declaration);
        assert_eq!(
            bootstrap(krusty),
            bootstrap(reference),
            "{class}: {declaration}"
        );
    }
}

/// A suspend lambda is a class of its own: it stores the receiver it captures in a field named the
/// same way, and its constructor takes it under that name rather than `$receiver`.
const SUSPEND_LAMBDAS: &str = "@JvmInline value class U(val s: String) {\n\
    \x20   init {\n\
    \x20       val f: suspend () -> Any = { this }\n\
    \x20   }\n\
    \x20   fun ms(): suspend () -> Any = { this }\n\
    }\n";

#[test]
fn a_value_class_suspend_lambda_stores_its_captured_receiver_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Suspends.kt", SUSPEND_LAMBDAS)]);
    for (class, constructor) in [
        (
            "U$f$1",
            "U$f$1(java.lang.String, kotlin.coroutines.Continuation<? super U$f$1>);",
        ),
        (
            "U$ms$1",
            "U$ms$1(java.lang.String, kotlin.coroutines.Continuation<? super U$ms$1>);",
        ),
    ] {
        // The constructor names the parameter and the field it stores it in (`putfield`).
        let (reference, krusty) = classes.method_listing(class, constructor);
        assert_eq!(krusty, reference, "{class}: {constructor}");
    }
}

/// A lambda `LambdaMetafactory` cannot adapt, such as one returning the value class, is a class of
/// its own. Its field and constructor parameter carry the captured receiver under the same name as
/// a lifted method's parameter (`$arg0`, `$tmp0`, `$tmp0_$this`), not `this$0` and `$receiver`,
/// and in capture order among the captured values. A lambda written in an `init` block is enclosed
/// by the primary `constructor-impl`, which runs it.
const LAMBDA_CLASSES: &str = "@JvmInline value class Tag(val s: String)\n\
    @JvmInline value class U(val s: String) {\n\
    \x20   init {\n\
    \x20       val f: () -> U = { this }\n\
    \x20       f()\n\
    \x20   }\n\
    \x20   constructor(t: Tag, u: Tag) : this(u.s) {\n\
    \x20       val g: () -> U = { this }\n\
    \x20       g()\n\
    \x20   }\n\
    \x20   fun mv(): () -> U = { this }\n\
    \x20   val pv: () -> U get() = { this }\n\
    \x20   fun mx(x: Int): () -> U = { if (x > 0) this else this }\n\
    \x20   fun both(t: Tag): () -> Tag = { Tag(t.s + this.s) }\n\
    \x20   fun inLocal(): () -> U {\n\
    \x20       fun loc(): () -> U = { this }\n\
    \x20       return loc()\n\
    \x20   }\n\
    }\n\
    class Holder(val s: String) {\n\
    \x20   fun m(): () -> U = { U(s) }\n\
    }\n\
    fun U.ext(): () -> U = { this }\n";

#[test]
fn a_value_class_lambda_class_stores_its_captured_receiver_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("LambdaClasses.kt", LAMBDA_CLASSES)]);
    // An ordinary class's instance stays `this$0`, passed as `$receiver`, and an extension
    // receiver `$this_ext`: only a value-class static's receiver is a captured value.
    for class in [
        "U$f$1",
        "U$g$1",
        "U$mv$1",
        "U$pv$1",
        "U$mx$1",
        "U$both$1",
        "U$inLocal$loc$1",
        "Holder$m$1",
        "LambdaClassesKt$ext$1",
    ] {
        let reference = classes
            .reference
            .get(class)
            .unwrap_or_else(|| panic!("kotlinc writes {class}"));
        let krusty = classes
            .krusty
            .get(class)
            .unwrap_or_else(|| panic!("krusty writes {class}"));
        assert_eq!(
            common::member_table(krusty),
            common::member_table(reference),
            "{class}: kotlinc's fields and methods"
        );
        let (reference, krusty) = classes.class_listing(class);
        assert_eq!(
            krusty, reference,
            "{class}: kotlinc's code and debug tables"
        );
    }
}

/// An anonymous object or local class written in a value class is lifted with the member's static
/// too, so the receiver it captures is that static's value: its field, constructor parameter and
/// local-variable row are `$arg0` in a member's or accessor's `-impl`, `$tmp0` in the primary
/// `constructor-impl` and `$tmp0_$this` in a secondary one, also through an enclosing lambda or
/// local function. An ordinary class's instance stays `this$0`, passed as `$receiver`.
const OBJECTS: &str = "interface Box { fun get(): Any }\n\
    @JvmInline value class Tag(val s: String)\n\
    @JvmInline value class O(val s: String) {\n\
    \x20   init {\n\
    \x20       val o = object : Box { override fun get(): Any = this@O }\n\
    \x20       o.get()\n\
    \x20   }\n\
    \x20   init {\n\
    \x20       fun li(): Any = object : Box { override fun get(): Any = this@O }\n\
    \x20       li()\n\
    \x20   }\n\
    \x20   constructor(t: Tag, u: Tag) : this(u.s) {\n\
    \x20       val o2 = object : Box { override fun get(): Any = this@O }\n\
    \x20       o2.get()\n\
    \x20   }\n\
    \x20   fun m(): Box = object : Box { override fun get(): Any = this@O }\n\
    \x20   val p: Box get() = object : Box { override fun get(): Any = this@O }\n\
    \x20   var q: Box\n\
    \x20       get() = object : Box { override fun get(): Any = this@O }\n\
    \x20       set(v) { object : Box { override fun get(): Any = this@O }.get() }\n\
    \x20   fun mx(x: Int): Box = object : Box { override fun get(): Any = this@O.s + x }\n\
    \x20   fun loc(): Box {\n\
    \x20       class L : Box { override fun get(): Any = this@O }\n\
    \x20       return L()\n\
    \x20   }\n\
    \x20   fun inLam(): () -> Box = { object : Box { override fun get(): Any = this@O } }\n\
    \x20   fun inLoc(): Box {\n\
    \x20       fun lf(): Box = object : Box { override fun get(): Any = this@O }\n\
    \x20       return lf()\n\
    \x20   }\n\
    }\n\
    class Plain(val s: String) {\n\
    \x20   fun m(): Box = object : Box { override fun get(): Any = this@Plain }\n\
    }\n";

#[test]
fn a_value_class_anonymous_object_stores_its_captured_receiver_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Objects.kt", OBJECTS)]);
    for class in [
        "O$o$1",
        "O$li$1",
        "O$o2$1",
        "O$m$1",
        "O$p$1",
        "O$q$1",
        "O$q$2",
        "O$mx$1",
        "O$loc$L",
        "O$inLam$1$1",
        "O$inLoc$lf$1",
        "Plain$m$1",
    ] {
        let (reference, krusty) = classes.class_listing(class);
        assert_eq!(
            krusty, reference,
            "{class}: kotlinc's fields, code, debug tables and enclosing method"
        );
    }
}

/// kotlinc lowers a value class's member or accessor to a static before it lifts what the
/// declaration contains, so the declaration's own extension receiver is an ordinary parameter of
/// that static, `$this$mext`, which a lambda, suspend lambda, anonymous object or local class
/// captures like any value: its field and constructor parameter are both `$this_mext`, where an
/// ordinary class's member extension passes its field as `$receiver`. A local extension function is
/// lifted, not lowered to a static, so its receiver stays a receiver, passed as `$receiver`. A
/// suspend lambda is no source class: an object it declares captures what the lambda captured, the
/// value-class static's `$arg0`.
const MEMBER_EXTENSIONS: &str = "interface Box { fun get(): Any }\n\
    class Two(val first: Any, val second: Any)\n\
    @JvmInline value class Tag(val s: String)\n\
    @JvmInline value class W(val s: String) {\n\
    \x20   fun Tag.mext(): () -> W = { W(this.s + this@W.s) }\n\
    \x20   val Tag.pe: () -> W get() = { W(this.s + this@W.s) }\n\
    \x20   fun Tag.msus(): suspend () -> Two = { Two(this, this@W) }\n\
    \x20   fun Tag.mo(): Box = object : Box { override fun get(): Any = this@mo }\n\
    \x20   val Tag.extObj: Box get() = object : Box { override fun get(): Any = this@extObj }\n\
    \x20   fun Tag.mlc(): Box {\n\
    \x20       class LL : Box { override fun get(): Any = this@mlc }\n\
    \x20       return LL()\n\
    \x20   }\n\
    \x20   fun Tag.inLoc(): Box {\n\
    \x20       fun lf(): Box = object : Box { override fun get(): Any = this@inLoc }\n\
    \x20       return lf()\n\
    \x20   }\n\
    \x20   fun m(): Any {\n\
    \x20       fun Tag.loc(): () -> W = { W(this.s + this@W.s) }\n\
    \x20       return Tag(\"\").loc()\n\
    \x20   }\n\
    \x20   fun su(): suspend () -> Box = { object : Box { override fun get(): Any = this@W } }\n\
    \x20   fun Tag.esu(): suspend () -> Box = { object : Box { override fun get(): Any = this@esu } }\n\
    }\n\
    class Plain(val s: String) {\n\
    \x20   fun Tag.pext(): () -> W = { W(this.s + this@Plain.s) }\n\
    \x20   fun Tag.po(): Box = object : Box { override fun get(): Any = this@po }\n\
    }\n";

#[test]
fn a_value_class_member_extension_receiver_capture_is_a_captured_value_like_kotlinc() {
    let classes =
        common::classes_against_kotlinc_module(&[("MemberExtensions.kt", MEMBER_EXTENSIONS)]);
    for class in [
        "W$mext$1",
        "W$pe$1",
        "W$mo$1",
        "W$extObj$1",
        "W$mlc$LL",
        "W$inLoc$lf$1",
        "W$m$loc$1",
        "Plain$pext$1",
        "Plain$po$1",
    ] {
        let (reference, krusty) = classes.class_listing(class);
        assert_eq!(
            krusty, reference,
            "{class}: kotlinc's fields, code, debug tables and enclosing method"
        );
    }
    // A suspend lambda's annotations are laid out from its own pool, and krusty orders the
    // `InnerClasses` rows of an object declared in one apart from kotlinc; those classes are
    // compared by their members and their constructor: the fields it stores and their
    // local-variable rows.
    for (class, constructor) in [
        (
            "W$msus$1",
            "W$msus$1(java.lang.String, java.lang.String, kotlin.coroutines.Continuation<? super W$msus$1>);",
        ),
        ("W$su$1$1", "W$su$1$1(java.lang.String);"),
        ("W$esu$1$1", "W$esu$1$1(java.lang.String);"),
    ] {
        let reference = classes
            .reference
            .get(class)
            .unwrap_or_else(|| panic!("kotlinc writes {class}"));
        let krusty = classes
            .krusty
            .get(class)
            .unwrap_or_else(|| panic!("krusty writes {class}"));
        assert_eq!(
            common::member_table(krusty),
            common::member_table(reference),
            "{class}: kotlinc's fields and methods"
        );
        let (reference, krusty) = classes.method_listing(class, constructor);
        assert_eq!(krusty, reference, "{class}: {constructor}");
    }
}
