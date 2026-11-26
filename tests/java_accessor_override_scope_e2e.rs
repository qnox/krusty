//! A Java method that overrides a Kotlin property's accessor is that property's accessor, not a
//! function of the Java classifier, as kotlinc's Java scope reads it.
use super::common;

/// A Java method overriding a Kotlin property's getter is that property's accessor, not a function
/// of the Java class: a Kotlin `getA()` below it overrides only the Java interface's `getA()`.
#[test]
fn a_java_override_of_a_kotlin_property_getter_is_no_function() {
    let x = "public interface X {\n    X getA();\n}\n";
    let ax = "\
public abstract class AX extends A implements X {
    @Override
    public AX getA() {
        return (AX) super.getA();
    }
}
";
    let source = "\
abstract class A {
    open val a: A? get() = null
}
class B : AX() {
    override fun getA(): X? = super.a
}
class E : AX() {
    override fun getA(): Int = 1
}
fun read(x: AX): X? = x.getA()
";
    let sources = [("X.java", x), ("AX.java", ax), ("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:8:26: return type of 'fun getA(): Int' is not a subtype of the return \
          type of the overridden member 'fun getA(): X!' defined in 'X'."
        ]
    );
}

/// A Kotlin property implementing a Java interface's accessors leaves no `getSetter()` or
/// `isDelegated()` function on the Kotlin class.
#[test]
fn a_kotlin_property_over_java_accessors_leaves_no_function() {
    let descriptor = "\
public interface PropertyDescriptor extends DescriptorWithAccessor {
    String getSetter();
    boolean isDelegated();
}
";
    let source = "\
interface DescriptorWithAccessor {
    val setter: String
    val isDelegated: Boolean
}

class WrappedPropertyDescriptor : PropertyDescriptor {
    override val setter: String get() = \"K\"
    override val isDelegated: Boolean get() = false
}

fun test() {
    val descriptor = WrappedPropertyDescriptor()
    val res1 = descriptor.setter
    val res2 = descriptor.getSetter()
    val res3 = descriptor.isDelegated
    val res4 = descriptor.isDelegated()
}
";
    let sources = [("PropertyDescriptor.java", descriptor), ("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:14:27: unresolved reference 'getSetter' on receiver of type \
             'WrappedPropertyDescriptor'.",
            "test.kt:16:27: expression 'isDelegated' of type 'Boolean' cannot be invoked as a \
             function. Function 'invoke()' is not found.",
        ]
    );
}

/// For a Kotlin `var`, a Java class overrides the property only when it overrides both accessors;
/// one of them alone stays an ordinary function. An accessor-named overload of another shape
/// (`String setA(String)`, `getA(int)`) is never part of the property and stays callable.
#[test]
fn a_java_overload_or_lone_accessor_of_a_kotlin_var_stays_a_function() {
    let lone = "\
public class Lone extends Base {
    @Override
    public void setA(int value) { super.setA(value + 1); }
}
";
    let both = "\
public class Both extends Base {
    @Override
    public int getA() { return 1; }
    @Override
    public void setA(int value) { super.setA(value + 1); }
}
";
    let derived = "\
public class Derived extends Base {
    public String setA(String value) { return value; }
    public int getA(int offset) { return offset; }
    @Override
    public int getA() { return 1; }
    @Override
    public void setA(int value) { super.setA(value + 1); }
}
";
    let source = "\
open class Base { open var a: Int = 0 }
fun lone(l: Lone) = l.setA(3)
fun use(d: Derived): String {
    val text: String = d.setA(\"OK\")
    d.setA(3)
    return text + d.getA(2) + d.a
}
fun both(b: Both) = b.getA()
";
    let sources = [
        ("Lone.java", lone),
        ("Both.java", both),
        ("Derived.java", derived),
        ("test.kt", source),
    ];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:5:12: argument type mismatch: actual type is 'Int', but 'String!' was \
             expected.",
            "test.kt:8:23: unresolved reference 'getA' on receiver of type 'Both'.",
        ]
    );
}

#[test]
fn a_superclass_private_property_leaves_the_java_getter_a_function() {
    let derived = "\
public class Derived extends Base { public String getA() { return \"OK\"; } }
";
    let source = "\
open class Base { private val a: String get() = \"base\" }
fun use(d: Derived): String = d.getA()
";
    common::assert_sources_accepted_like_kotlinc(
        &[("Derived.java", derived), ("test.kt", source)],
        &[],
    );
}
