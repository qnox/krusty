//! kotlinc reaches an inherited member through its fake override in the receiver's static class.
//! An inherited field (`@JvmField`) is therefore named on the receiver's class (`C.n`, not
//! `P.n`), and inside the declaring class a property read through a subclass-typed receiver is
//! that subclass's accessor (`C.getX()`), not the declaring class's private backing field.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const INHERITED_FIELDS: &str = "\
open class P {
    @JvmField var n: Int = 0
}

open class C : P() {
    fun own(): Int = n
}

class G : C()

class Other {
    fun read(c: C): Int = c.n
    fun readLeaf(g: G): Int = g.n
    fun write(c: C) {
        c.n = 2
    }
    fun readDeclared(p: P): Int = p.n
}
";

/// Reads and writes of an inherited `@JvmField` name the receiver's class; a receiver of the
/// declaring class names the declaring class.
#[test]
fn an_inherited_jvm_field_is_named_on_the_receivers_class() {
    assert_like_kotlinc(&[("Fields.kt", INHERITED_FIELDS)]);
}

const FIELDS_DECLARED: &str = "\
open class Base {
    @JvmField var count: Int = 0
}
";

const FIELDS_USED: &str = "\
class Derived : Base() {
    fun bump(): Int {
        count++
        return count
    }
}

fun total(d: Derived): Int = d.count
";

/// The same holds when the field is declared in another file of the module.
#[test]
fn an_inherited_jvm_field_of_another_file_is_named_on_the_receivers_class() {
    assert_like_kotlinc(&[("Base.kt", FIELDS_DECLARED), ("Derived.kt", FIELDS_USED)]);
}

const SUBCLASS_RECEIVER_IN_CLASS: &str = "\
open class P {
    val x: Int = 1
    var y: Int = 2
    internal val z: Int = 3
    lateinit var late: String

    fun read(c: C): Int = c.x + c.z
    fun write(c: C) {
        c.y = 5
    }
    fun lateOf(c: C): String = c.late
    fun own(p: P): Int = p.x + x
}

open class C : P()
";

/// Inside the declaring class, a property of a subclass-typed receiver is read and written
/// through the subclass's accessors; the declaring class's own receiver keeps the field.
#[test]
fn a_subclass_receiver_inside_the_declaring_class_uses_its_accessors() {
    assert_like_kotlinc(&[("Receivers.kt", SUBCLASS_RECEIVER_IN_CLASS)]);
}

const JAVA_BASE: &str = "\
package jf;
public class JavaBase {
    public int open;
    protected int guarded;
}
";

const JAVA_FIELD_USE: &str = "\
import jf.JavaBase

class KotlinChild : JavaBase() {
    fun sum(): Int = open + guarded
}

fun read(child: KotlinChild): Int = child.open
";

/// An inherited Java field is named on the receiver's class like an inherited `@JvmField`.
#[test]
fn an_inherited_java_field_is_named_on_the_receivers_class() {
    let (library, _) =
        common::javac_compile(&[("JavaBase.java".to_string(), JAVA_BASE.to_string())], &[])
            .expect("javac compiles the Java base class");
    let classes =
        common::classes_against_kotlinc_module_cp(&[("Use.kt", JAVA_FIELD_USE)], &[library]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
