//! A local class whose SUPERCLASS is a local class with captures.
//!
//! A capturing local class takes its captures as synthetic PREFIX parameters of its constructor,
//! ahead of the ones the source wrote. A subclass's `super(…)` spells only the written ones — the
//! prefix is not in the source and there is no expression there to resolve — so two things had to
//! be supplied for such a subclass to be constructible at all: it has to CARRY the superclass's
//! captures (selection records them as its own, read from the resolved supertype rather than from a
//! call, because a supertype constructor call is not an expression), and its `super(…)` has to PASS
//! them on ahead of the written arguments.
//!
//! Without either half the call was one value short per capture, and the emitted class was rejected
//! with `VerifyError: Bad type on operand stack` — `this` sat where the capture belonged. kotlinc
//! compiles every program here, so every expectation is taken by running it under kotlinc.
//!
use super::common;

/// Run `body` under krusty AND under the reference compiler, and require the SAME output.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    let reference = common::kotlinc_box_result(body);
    assert_eq!(reference, "OK", "{stem}: unexpected kotlinc result");
    assert_eq!(krusty, reference, "{stem}: krusty and kotlinc disagree");
}

/// The subclass captures NOTHING of its own: everything it carries is the superclass's.
#[test]
fn a_local_subclass_carries_its_superclasss_capture() {
    agrees_with_kotlinc(
        "LocalSubclassCarriesCapture",
        "fun foo(s: String): String {\n\
         \x20   open class Local {\n\
         \x20       fun f() = s\n\
         \x20   }\n\
         \x20   open class Derived : Local() {\n\
         \x20       fun g() = f()\n\
         \x20   }\n\
         \x20   return Derived().g()\n\
         }\n\
         fun box(): String = foo(\"OK\")\n",
    );
}

/// The superclass takes a WRITTEN argument beside its capture, so the prefix goes ahead of it.
#[test]
fn a_captured_prefix_precedes_the_written_super_arguments() {
    agrees_with_kotlinc(
        "CapturedPrefixPrecedesWritten",
        "fun box(): String {\n\
         \x20   val result = \"OK\"\n\
         \x20   open class Local(val ok: Boolean) {\n\
         \x20       fun result() = if (ok) result else \"Fail\"\n\
         \x20   }\n\
         \x20   class Derived : Local(true)\n\
         \x20   return Derived().result()\n\
         }\n",
    );
}

/// Both capture, and they capture DIFFERENT values: the subclass carries its own and the
/// superclass's, and passes on only the superclass's.
#[test]
fn a_subclass_with_its_own_capture_still_passes_the_superclasss() {
    agrees_with_kotlinc(
        "SubclassWithItsOwnCapture",
        "fun box(): String {\n\
         \x20   val left = \"O\"\n\
         \x20   val right = \"K\"\n\
         \x20   open class Local {\n\
         \x20       fun first() = left\n\
         \x20   }\n\
         \x20   class Derived : Local() {\n\
         \x20       fun both() = first() + right\n\
         \x20   }\n\
         \x20   return Derived().both()\n\
         }\n",
    );
}

/// The same value captured by both: one capture, not two, and the subclass passes it on.
#[test]
fn one_value_captured_by_both_is_carried_once() {
    agrees_with_kotlinc(
        "OneValueCapturedByBoth",
        "fun box(): String {\n\
         \x20   val shared = \"OK\"\n\
         \x20   open class Local {\n\
         \x20       fun base() = shared\n\
         \x20   }\n\
         \x20   class Derived : Local() {\n\
         \x20       fun mine() = shared\n\
         \x20   }\n\
         \x20   val derived = Derived()\n\
         \x20   return if (derived.base() == derived.mine()) derived.mine() else \"Fail\"\n\
         }\n",
    );
}

/// THREE levels, each link passing the capture to the next. The member is reached through the
/// middle class, because a call naming a member two levels up a LOCAL hierarchy does not resolve
/// today — a separate, pre-existing gap that has nothing to do with captures.
#[test]
fn a_capture_travels_a_three_level_local_hierarchy() {
    agrees_with_kotlinc(
        "CaptureTravelsThreeLevels",
        "fun box(): String {\n\
         \x20   val result = \"OK\"\n\
         \x20   open class First {\n\
         \x20       fun value() = result\n\
         \x20   }\n\
         \x20   open class Second : First() {\n\
         \x20       fun middle() = value()\n\
         \x20   }\n\
         \x20   class Third : Second() {\n\
         \x20       fun last() = middle()\n\
         \x20   }\n\
         \x20   return Third().last()\n\
         }\n",
    );
}

/// A MUTABLE capture is a shared cell, and the subclass must pass on the cell rather than a copy:
/// a write through the enclosing function is visible to the superclass's read.
#[test]
fn a_mutable_capture_stays_one_cell_across_the_hierarchy() {
    agrees_with_kotlinc(
        "MutableCaptureAcrossHierarchy",
        "fun box(): String {\n\
         \x20   var state = \"Fail\"\n\
         \x20   open class Local {\n\
         \x20       fun read() = state\n\
         \x20   }\n\
         \x20   class Derived : Local()\n\
         \x20   val derived = Derived()\n\
         \x20   state = \"OK\"\n\
         \x20   return derived.read()\n\
         }\n",
    );
}

/// The subclass calls a no-argument SECONDARY constructor. That `<init>` still takes the capture
/// ahead of its empty source parameter list; calling `A.<init>()` does not resolve.
#[test]
fn a_subclass_passes_a_capture_to_a_secondary_constructor() {
    agrees_with_kotlinc(
        "SecondaryConstructorCapture",
        "fun box(): String {\n\
         \x20   val z = \"K\"\n\
         \x20   open class A(val x: String) {\n\
         \x20       constructor() : this(\"O\")\n\
         \x20       val y: String\n\
         \x20           get() = z\n\
         \x20   }\n\
         \x20   class B : A()\n\
         \x20   val b = B()\n\
         \x20   return b.x + b.y\n\
         }\n",
    );
}

/// A local superclass with no primary constructor carries the same capture prefix on the selected
/// super-delegating secondary constructor; there is no primary parameter list to borrow it from.
#[test]
fn a_subclass_uses_the_selected_secondary_constructor_capture_prefix() {
    agrees_with_kotlinc(
        "SecondaryOnlyConstructorCapture",
        "fun box(): String {\n\
         \x20   val result = \"OK\"\n\
         \x20   open class CapturingBase {\n\
         \x20       constructor()\n\
         \x20       fun read() = result\n\
         \x20   }\n\
         \x20   class Derived : CapturingBase()\n\
         \x20   return Derived().read()\n\
         }\n",
    );
}

/// An inner class does not grow a constructor parameter for a superclass capture the enclosing
/// local class already stores. The super call reads that field from the enclosing-instance
/// parameter, ahead of the arguments written in source.
#[test]
fn an_inner_class_reads_a_superclass_capture_from_the_enclosing_instance() {
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let (files, diagnostics) = common::capture_common_ir(
        "fun String.bar(): String {\n\
         \x20   open class Local(val extra: String) {\n\
         \x20       fun result() = this@bar + extra\n\
         \x20   }\n\
         \x20   class Outer {\n\
         \x20       inner class Inner : Local(\"K\") {\n\
         \x20           fun outer() = this@Outer\n\
         \x20       }\n\
         \x20   }\n\
         \x20   return Outer().Inner().result()\n\
         }\n\
         fun box() = \"O\".bar()\n",
        "InnerSuperEnclosingCapture",
        platform,
    );
    assert!(diagnostics.is_empty(), "frontend rejected: {diagnostics:?}");
    let ir = files.into_iter().next().expect("one lowered file");
    let inner_index = ir
        .classes
        .iter()
        .position(|class| class.is_inner_class)
        .expect("inner class");
    let inner = &ir.classes[inner_index];
    let parent = ir
        .class_id_by_name(inner.superclass)
        .expect("local superclass is in this file");
    let enclosing = inner
        .ctor_args
        .iter()
        .find(|argument| {
            argument.provenance == krusty::ir::IrCtorParameterProvenance::EnclosingInstance
        })
        .expect("enclosing-instance parameter");
    let outer = ir
        .class_id_by_name(enclosing.ty.obj_internal().expect("enclosing class"))
        .expect("enclosing class");
    let describe = |class: krusty::ir::ClassId| {
        ir.classes[class as usize]
            .ctor_args
            .iter()
            .map(|argument| {
                format!(
                    "field={:?} ty={:?} capture={:?}",
                    argument.field_index, argument.ty, argument.capture
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        inner.constructor_prefix_count,
        1,
        "inner keeps only the enclosing instance\ninner {:?}\nparent {:?}\nouter {:?}",
        describe(inner_index as u32),
        describe(parent),
        describe(outer),
    );
    let [field_read, written] = inner.super_args.as_slice() else {
        panic!(
            "super arguments {:?}\nparent {:?}\nouter {:?}",
            inner
                .super_args
                .iter()
                .map(|argument| format!("{:?}", ir.expr(*argument)))
                .collect::<Vec<_>>(),
            describe(parent),
            describe(outer),
        );
    };
    let krusty::ir::IrExpr::GetField {
        receiver,
        class,
        index,
    } = ir.expr(*field_read)
    else {
        panic!("leading super argument {:?}", ir.expr(*field_read));
    };
    assert!(
        matches!(ir.expr(*receiver), krusty::ir::IrExpr::GetValue(1)),
        "the field is read from the enclosing-instance parameter, not from this: {:?}",
        ir.expr(*receiver)
    );
    assert_eq!(*class, outer);
    let outer_argument = ir.classes[outer as usize]
        .ctor_args
        .iter()
        .find(|argument| argument.field_index == Some(*index))
        .expect("enclosing capture field");
    let capture = outer_argument.capture.as_ref();
    assert!(
        matches!(
            capture,
            Some(krusty::ir::IrConstructorCapture {
                receiver: Some(krusty::ir::IrCapturedReceiver::Callable(label)),
                ..
            }) if label.as_ref() == "bar"
        ),
        "{capture:?}"
    );
    assert!(matches!(
        ir.expr(*written),
        krusty::ir::IrExpr::Const(krusty::ir::IrConst::String(text)) if text.as_str() == Some("K")
    ));
}

/// The same program kotlinc runs: the inner constructor passes the outer class's captured
/// extension receiver into the local superclass and `box` returns that receiver.
#[test]
fn an_inner_local_class_returns_the_captured_extension_receiver() {
    agrees_with_kotlinc(
        "InnerOfLocalCaptureExtensionReceiver",
        "fun String.bar(): String {\n\
         \x20   open class Local {\n\
         \x20       fun result() = this@bar\n\
         \x20   }\n\
         \x20   class Outer {\n\
         \x20       inner class Inner : Local() {\n\
         \x20           fun outer() = this@Outer\n\
         \x20       }\n\
         \x20   }\n\
         \x20   return Outer().Inner().result()\n\
         }\n\
         fun box() = \"OK\".bar()\n",
    );
}

/// A function and a lambda spelled `bar` are different receivers. The superclass reads the lambda.
/// The enclosing class also captures the function's `Host` receiver, and that field is not the
/// super argument.
#[test]
fn same_spelled_extension_receivers_do_not_cross_on_the_enclosing_instance() {
    agrees_with_kotlinc(
        "DistinctExtensionReceiverLabels",
        "class Host(val mark: String)\n\
         fun Host.bar(): String {\n\
         \x20   return \"OK\".run bar@{\n\
         \x20       open class Local {\n\
         \x20           fun read() = this@bar\n\
         \x20       }\n\
         \x20       class Holder {\n\
         \x20           fun fromHost() = mark\n\
         \x20           inner class Inner : Local()\n\
         \x20       }\n\
         \x20       val inner = Holder().Inner().read()\n\
         \x20       val seen = Holder().fromHost()\n\
         \x20       if (inner == \"OK\" && seen == \"NO\") \"OK\" else inner + seen\n\
         \x20   }\n\
         }\n\
         fun box() = Host(\"NO\").bar()\n",
    );
}

/// Two lambdas spelled `label` are different receivers. The superclass reads the inner `String`
/// lambda. The enclosing class also stores the outer `Host` lambda, which carries the same label.
#[test]
fn same_spelled_lambda_receivers_do_not_cross_on_the_enclosing_instance() {
    agrees_with_kotlinc(
        "DistinctLambdaReceiverLabels",
        "class Host(val mark: String)\n\
         fun box(): String {\n\
         \x20   return Host(\"NO\").run label@{\n\
         \x20       \"OK\".run label@{\n\
         \x20           open class Local {\n\
         \x20               fun read() = this@label\n\
         \x20           }\n\
         \x20           class Holder {\n\
         \x20               fun fromHost() = mark\n\
         \x20               inner class Inner : Local()\n\
         \x20           }\n\
         \x20           val seen = Holder().fromHost()\n\
         \x20           val inner = Holder().Inner().read()\n\
         \x20           if (inner == \"OK\" && seen == \"NO\") \"OK\" else inner + seen\n\
         \x20       }\n\
         \x20   }\n\
         }\n",
    );
}

/// A local subclass with only secondary constructors forwards the superclass capture prefix on
/// each `super(…)`. `this(…)` keeps the subclass's own prefix.
#[test]
fn a_local_subclass_secondary_constructor_forwards_superclass_captures() {
    // secondaryConstructors/localClasses.kt: both classes are local and have only secondary
    // constructors. A's `super(x1, x2)` must pass B's captures ahead of the written ints, and the
    // init blocks still run before each constructor body.
    agrees_with_kotlinc(
        "LocalSecondaryConstructors",
        r##"
open class C(val grandParentProp: String)
fun box(): String {
    var sideEffects: String = ""
    var parentSideEffects: String = ""
    val justForUsageInClosure = 7
    val justForUsageInParentClosure = "parentCaptured"

    abstract class B : C {
        val parentProp: String
        init {
            sideEffects += "minus-one#"
            parentSideEffects += "1"
        }
        protected constructor(arg: Int): super(justForUsageInParentClosure) {
            parentProp = (arg).toString()
            sideEffects += "0.5#"
            parentSideEffects += "#" + justForUsageInParentClosure
        }
        protected constructor(arg1: Int, arg2: Int): super(justForUsageInParentClosure) {
            parentProp = (arg1 + arg2).toString()
            sideEffects += "0.7#"
            parentSideEffects += "#3"
        }
        init {
            sideEffects += "zero#"
            parentSideEffects += "#4"
        }
    }

    class A : B {
        var prop: String = ""
        init {
            sideEffects += prop + "first"
        }

        constructor(x1: Int, x2: Int): super(x1, x2) {
            prop = x1.toString()
            sideEffects += "#third"
        }

        init {
            sideEffects += prop + "#second"
        }

        constructor(x: Int): super(justForUsageInClosure + x) {
            prop += "${x}#int"
            sideEffects += "#fourth"
        }

        constructor(): this(justForUsageInClosure) {
            sideEffects += "#fifth"
        }

        override fun toString() = "$prop#$parentProp#$grandParentProp"
    }

    val a1 = A(5, 10).toString()
    if (a1 != "5#15#parentCaptured") return "fail1: $a1"
    if (sideEffects != "minus-one#zero#0.7#first#second#third") return "fail2: ${sideEffects}"
    if (parentSideEffects != "1#4#3") return "fail3: ${parentSideEffects}"

    sideEffects = ""
    parentSideEffects = ""
    val a2 = A(123).toString()
    if (a2 != "123#int#130#parentCaptured") return "fail1: $a2"
    if (sideEffects != "minus-one#zero#0.5#first#second#fourth") return "fail4: ${sideEffects}"
    if (parentSideEffects != "1#4#parentCaptured") return "fail5: ${parentSideEffects}"

    sideEffects = ""
    parentSideEffects = ""
    val a3 = A().toString()
    if (a3 != "7#int#14#parentCaptured") return "fail6: $a3"
    if (sideEffects != "minus-one#zero#0.5#first#second#fourth#fifth") return "fail7: ${sideEffects}"
    if (parentSideEffects != "1#4#parentCaptured") return "fail8: ${parentSideEffects}"

    return "OK"
}
"##,
    );
}

/// The secondary `super(n)` descriptor carries the superclass capture prefix, in capture order,
/// ahead of the written `Int`. The observation is each local class's `<init>` access and JVM
/// descriptor, not a value's `toString` and not the generic Signature attribute.
#[test]
fn a_secondary_super_call_keeps_the_capture_prefix_in_its_descriptor() {
    let source = "fun box(): String {\n\
         \x20   val kept = \"K\"\n\
         \x20   var changed = \"C\"\n\
         \x20   abstract class Base {\n\
         \x20       constructor(n: Int) { changed = n.toString() }\n\
         \x20       fun read(): String = kept + changed\n\
         \x20   }\n\
         \x20   class Child : Base {\n\
         \x20       constructor(n: Int): super(n)\n\
         \x20   }\n\
         \x20   val child = Child(2)\n\
         \x20   return if (child.read() == \"K2\") \"OK\" else child.read()\n\
         }\n";
    let classes = common::classes_against_kotlinc_module(&[("CapturePrefix.kt", source)]);
    let constructors = |suffix: &str| {
        let name = classes
            .reference
            .keys()
            .find(|name| name.ends_with(suffix))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "kotlinc wrote no {suffix}: {:?}",
                    classes.reference.keys().collect::<Vec<_>>()
                )
            });
        let descriptors = |bytes: &[u8]| {
            common::member_table(bytes)
                .into_iter()
                .filter_map(|member| {
                    let method = member.strip_prefix("method ")?;
                    let (access, rest) = method.split_once(' ')?;
                    let (name_desc, _) = rest.split_once(' ')?;
                    name_desc
                        .starts_with("<init>")
                        .then(|| format!("{access} {name_desc}"))
                })
                .collect::<Vec<_>>()
        };
        let reference = descriptors(&classes.reference[&name]);
        let krusty_bytes = classes
            .krusty
            .get(&name)
            .unwrap_or_else(|| panic!("krusty wrote no {name}"));
        let krusty = descriptors(krusty_bytes);
        (name, reference, krusty)
    };
    let (base, reference, krusty) = constructors("$Base");
    assert_eq!(krusty, reference, "{base} constructors");
    let (child, reference, krusty) = constructors("$Child");
    assert_eq!(krusty, reference, "{child} constructors");
}

/// Anonymous-object capture discovery uses the same resolved superclass edge. The object body does
/// not mention `result`; it carries that value solely because its local superclass requires it.
#[test]
fn an_anonymous_object_passes_on_its_local_superclasss_capture() {
    agrees_with_kotlinc(
        "AnonymousObjectPassesCapture",
        "fun box(): String {\n\
         \x20   val result = \"OK\"\n\
         \x20   open class Local {\n\
         \x20       fun value() = result\n\
         \x20   }\n\
         \x20   return object : Local() {}.value()\n\
         }\n",
    );
}
