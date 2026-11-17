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
