//! kotlinc's Parcelize compiler plugin, as krusty's native pass.
//!
//! `-Xplugin=parcelize-compiler.jar` makes every `@Parcelize` class that implements
//! `android.os.Parcelable` parcelable: the frontend declares `describeContents()` and
//! `writeToParcel(Parcel, Int)`, and the transform gives them bodies and adds the static `CREATOR`
//! with its local `Creator` class. The fixtures build against small `android.os` stubs whose
//! `Parcel` really stores values, so `box()` writes each class to a parcel and reads it back
//! through its `CREATOR`. Every class krusty emits must equal kotlinc's byte for byte.

use std::path::PathBuf;

use super::common;
use super::compiler_plugin_fixture::{
    assert_same_classes_and_box, kotlinc_plugin_jar, PluginFixture,
};

const PARCELABLE: &str = r#"package android.os;

public interface Parcelable {
    int CONTENTS_FILE_DESCRIPTOR = 1;
    int PARCELABLE_WRITE_RETURN_VALUE = 1;
    int describeContents();
    void writeToParcel(Parcel dest, int flags);
    interface Creator<T> {
        T createFromParcel(Parcel source);
        T[] newArray(int size);
    }
}
"#;

const PARCEL: &str = r#"package android.os;

import java.util.ArrayList;

public final class Parcel {
    private final ArrayList<Object> values = new ArrayList<>();
    private int position;

    public static Parcel obtain() { return new Parcel(); }
    public void setDataPosition(int position) { this.position = position; }
    public int dataSize() { return values.size(); }

    private Object next() { return values.get(position++); }

    public void writeString(String value) { values.add(value); }
    public String readString() { return (String) next(); }
    public void writeByte(byte value) { values.add(value); }
    public byte readByte() { return (Byte) next(); }
    public void writeInt(int value) { values.add(value); }
    public int readInt() { return (Integer) next(); }
    public void writeLong(long value) { values.add(value); }
    public long readLong() { return (Long) next(); }
    public void writeFloat(float value) { values.add(value); }
    public float readFloat() { return (Float) next(); }
    public void writeDouble(double value) { values.add(value); }
    public double readDouble() { return (Double) next(); }
    public void writeIntArray(int[] value) { values.add(value == null ? null : value.clone()); }
    public int[] createIntArray() { return (int[]) next(); }
    public void writeByteArray(byte[] value) { values.add(value == null ? null : value.clone()); }
    public byte[] createByteArray() { return (byte[]) next(); }
    public void writeLongArray(long[] value) { values.add(value == null ? null : value.clone()); }
    public long[] createLongArray() { return (long[]) next(); }
    public void writeFloatArray(float[] value) { values.add(value == null ? null : value.clone()); }
    public float[] createFloatArray() { return (float[]) next(); }
    public void writeDoubleArray(double[] value) { values.add(value == null ? null : value.clone()); }
    public double[] createDoubleArray() { return (double[]) next(); }
    public void writeBooleanArray(boolean[] value) { values.add(value == null ? null : value.clone()); }
    public boolean[] createBooleanArray() { return (boolean[]) next(); }
    public void writeCharArray(char[] value) { values.add(value == null ? null : value.clone()); }
    public char[] createCharArray() { return (char[]) next(); }
    public void writeParcelable(Parcelable value, int flags) { values.add(value); }
    public <T extends Parcelable> T readParcelable(ClassLoader loader) { return (T) next(); }
}
"#;

/// The `android.os` stubs, compiled once per test, and the Parcelize runtime's annotations.
fn android_classpath() -> Vec<PathBuf> {
    let sources = [
        ("Parcelable.java".to_string(), PARCELABLE.to_string()),
        ("Parcel.java".to_string(), PARCEL.to_string()),
    ];
    let (stubs, _) =
        common::javac_compile(&sources, &[]).expect("javac compiles the android stubs");
    vec![stubs, kotlinc_plugin_jar("parcelize-runtime.jar")]
}

fn parcelize_switches() -> Vec<String> {
    vec![format!(
        "-Xplugin={}",
        kotlinc_plugin_jar("parcelize-compiler.jar").display()
    )]
}

const ROUND_TRIP: &str = r#"package demo

import android.os.Parcel
import android.os.Parcelable
import kotlinx.parcelize.Parcelize

@Parcelize
class User(
    val name: String,
    val nick: String?,
    val age: Int,
    val id: Long,
    val flag: Byte,
    val ratio: Float,
    val score: Double?,
    val level: Int?,
) : Parcelable

@Parcelize
class Scalars(
    val flag: Boolean,
    val maybeFlag: Boolean?,
    val small: Short,
    val maybeSmall: Short?,
    val letter: Char,
    val maybeLetter: Char?,
) : Parcelable

@Parcelize
class Unsigned(
    val byte: UByte,
    val short: UShort,
    val int: UInt,
    val long: ULong,
    val maybeInt: UInt?,
    val maybeLong: ULong?,
    val maybeByte: UByte?,
    val maybeShort: UShort?,
) : Parcelable

@Parcelize
class Arrays(
    val ints: IntArray,
    val bytes: ByteArray,
    val longs: LongArray?,
    val floats: FloatArray,
    val doubles: DoubleArray,
    val flags: BooleanArray?,
    val chars: CharArray,
) : Parcelable

enum class Color { RED, GREEN, BLUE }

@Parcelize
class Palette(val main: Color, val accent: Color?, val tag: String) : Parcelable

@Parcelize
class Inner(val x: Int) : Parcelable

@Parcelize
class Holder(val inner: Inner, val maybe: Inner?, val any: Parcelable, val maybeAny: Parcelable?) : Parcelable

class Outer {
    @Parcelize
    class Point(val x: Int, val y: Long?) : Parcelable
}

@Suppress("UNCHECKED_CAST")
fun <T> creator(type: Class<T>): Parcelable.Creator<T> =
    type.getField("CREATOR").get(null) as Parcelable.Creator<T>

fun <T : Parcelable> roundTrip(value: T, type: Class<T>): T {
    val parcel = Parcel.obtain()
    value.writeToParcel(parcel, 0)
    parcel.setDataPosition(0)
    return creator(type).createFromParcel(parcel)
}

fun ub(value: Int): UByte = value.toUByte()

fun us(value: Int): UShort = value.toUShort()

fun box(): String {
    val user = User("ann", null, 30, 7L, 1, 1.5f, 2.5, null)
    if (user.describeContents() != 0) return "describeContents"
    val back = roundTrip(user, User::class.java)
    if (back.name != "ann" || back.nick != null || back.age != 30 || back.id != 7L) return "user"
    if (back.flag != 1.toByte() || back.ratio != 1.5f) return "user primitives"
    if (back.score != 2.5 || back.level != null) return "user nullable"
    val other = roundTrip(User("bo", "b", 1, 2L, 3, 4f, null, 5), User::class.java)
    if (other.nick != "b" || other.score != null || other.level != 5) return "other"
    val point = roundTrip(Outer.Point(3, 4L), Outer.Point::class.java)
    if (point.x != 3 || point.y != 4L) return "point"
    if (roundTrip(Outer.Point(1, null), Outer.Point::class.java).y != null) return "point null"
    val scalars = roundTrip(Scalars(true, null, -7, 9, 'k', null), Scalars::class.java)
    if (!scalars.flag || scalars.maybeFlag != null) return "booleans"
    if (scalars.small != (-7).toShort() || scalars.maybeSmall != 9.toShort()) return "shorts"
    if (scalars.letter != 'k' || scalars.maybeLetter != null) return "chars"
    val others = roundTrip(Scalars(false, true, 0, null, '\u00e9', 'z'), Scalars::class.java)
    if (others.flag || others.maybeFlag != true || others.maybeSmall != null) return "other scalars"
    if (others.letter != '\u00e9' || others.maybeLetter != 'z') return "other chars"
    val unsigned = roundTrip(
        Unsigned(ub(200), us(60000), 4000000000u, 18000000000000000000uL, null, 5uL, ub(7), null),
        Unsigned::class.java,
    )
    if (unsigned.byte != 200.toUByte() || unsigned.short != 60000.toUShort()) return "small unsigned"
    if (unsigned.int != 4000000000u || unsigned.long != 18000000000000000000uL) return "unsigned"
    if (unsigned.maybeInt != null || unsigned.maybeLong != 5uL) return "nullable unsigned"
    if (unsigned.maybeByte != 7.toUByte() || unsigned.maybeShort != null) return "nullable small"
    val arrays = roundTrip(
        Arrays(
            intArrayOf(1, -2),
            byteArrayOf(3),
            null,
            floatArrayOf(1.5f),
            doubleArrayOf(2.5, 3.5),
            booleanArrayOf(true, false),
            charArrayOf('a', 'b'),
        ),
        Arrays::class.java,
    )
    if (!arrays.ints.contentEquals(intArrayOf(1, -2)) || !arrays.bytes.contentEquals(byteArrayOf(3))) {
        return "int and byte arrays"
    }
    if (arrays.longs != null || !arrays.floats.contentEquals(floatArrayOf(1.5f))) return "arrays"
    if (!arrays.doubles.contentEquals(doubleArrayOf(2.5, 3.5))) return "double array"
    if (!arrays.flags!!.contentEquals(booleanArrayOf(true, false))) return "boolean array"
    if (!arrays.chars.contentEquals(charArrayOf('a', 'b'))) return "char array"
    val palette = roundTrip(Palette(Color.GREEN, null, "p"), Palette::class.java)
    if (palette.main != Color.GREEN || palette.accent != null || palette.tag != "p") return "enum"
    if (roundTrip(Palette(Color.BLUE, Color.RED, ""), Palette::class.java).accent != Color.RED) {
        return "nullable enum"
    }
    val holder = roundTrip(Holder(Inner(1), null, Inner(2), Inner(3)), Holder::class.java)
    if (holder.inner.x != 1 || holder.maybe != null) return "parcelable"
    if ((holder.any as Inner).x != 2 || (holder.maybeAny as Inner).x != 3) return "parcelable interface"
    val users = creator(User::class.java).newArray(2)
    if (users.size != 2 || users[0] != null) return "newArray"
    return "OK"
}
"#;

/// Strings, primitives (booleans, shorts, chars, unsigned integers, primitive arrays and enums included) and their nullable forms, top-level and nested classes: `describeContents`,
/// `writeToParcel`, `CREATOR` and its local `Creator` class with their bridges, generic signature
/// and debug tables match kotlinc's, and each class reads back what it wrote.
#[test]
fn parcelized_classes_match_kotlinc_and_round_trip() {
    let fixture = PluginFixture::new("parcelize-round-trip");
    let sources = [("User.kt", ROUND_TRIP)];
    let switches = parcelize_switches();
    let classpath = android_classpath();
    let reference = fixture.kotlinc("main", &sources, &classpath, &switches);
    let krusty = fixture.krusty("main", &sources, &classpath, &switches);
    let mut runtime = classpath;
    runtime.push(common::stdlib_jar());
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}

const DATA_CLASS: &str = r#"package demo

import android.os.Parcel
import android.os.Parcelable
import kotlinx.parcelize.Parcelize

@Parcelize
data class Counter(val label: String, val count: Int) : Parcelable {
    fun bumped(): Counter = copy(count = count + 1)
}

fun box(): String {
    val parcel = Parcel.obtain()
    Counter("clicks", 2).bumped().writeToParcel(parcel, 0)
    parcel.setDataPosition(0)
    @Suppress("UNCHECKED_CAST")
    val back = Counter::class.java.getField("CREATOR").get(null) as Parcelable.Creator<Counter>
    val counter = back.createFromParcel(parcel)
    if (counter != Counter("clicks", 3)) return "round trip"
    if (counter.component1() != "clicks" || counter.component2() != 3) return "components"
    return "OK"
}
"#;

/// A `data class` is parcelable too: the members the plugin declares (`describeContents`,
/// `writeToParcel`) precede the `componentN`, `copy`, `toString`, `hashCode` and `equals` the
/// compiler generates for it, as in kotlinc, and the class reads back what it wrote.
#[test]
fn a_parcelized_data_class_matches_kotlinc_and_round_trips() {
    let fixture = PluginFixture::new("parcelize-data-class");
    let sources = [("Counter.kt", DATA_CLASS)];
    let switches = parcelize_switches();
    let classpath = android_classpath();
    let reference = fixture.kotlinc("main", &sources, &classpath, &switches);
    let krusty = fixture.krusty("main", &sources, &classpath, &switches);
    let mut runtime = classpath;
    runtime.push(common::stdlib_jar());
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}

const UNSUPPORTED: &str = r#"package demo

import android.os.Parcelable
import kotlinx.parcelize.Parcelize

@Parcelize
class Tags(val tags: List<String>) : Parcelable

@Parcelize
object Single : Parcelable
"#;

/// A class the port does not cover yet is refused with a diagnostic naming it, never compiled
/// without its parcelable members.
#[test]
fn unsupported_parcelized_classes_are_refused() {
    let fixture = PluginFixture::new("parcelize-unsupported");
    let sources = [("Tags.kt", UNSUPPORTED)];
    assert_eq!(
        fixture.krusty_refuses(
            "main",
            &sources,
            &android_classpath(),
            &parcelize_switches()
        ),
        "<fixture>/main/src/Tags.kt:7:1: error: krusty's Parcelize compiler plugin does not \
         support a property of this type yet; compile this module with kotlinc\n\
         <fixture>/main/src/Tags.kt:10:1: error: krusty's Parcelize compiler plugin does not \
         support a parcelized object, enum, interface or value class yet; compile this module \
         with kotlinc\n\
         krusty: 2 error(s)\n"
    );
}

const RAW_VALUE: &str = r#"package demo

import android.os.Parcelable
import kotlinx.parcelize.Parcelize
import kotlinx.parcelize.RawValue

@Parcelize
class Raw(
    val name: String,
    val tag: @RawValue String,
) : Parcelable
"#;

/// A runtime type-use annotation chooses another serializer; the frontend refuses the property
/// that carries it, since only its declared type shows it.
#[test]
fn a_runtime_type_annotation_is_refused_at_its_property() {
    let fixture = PluginFixture::new("parcelize-raw-value");
    let sources = [("Raw.kt", RAW_VALUE)];
    assert_eq!(
        fixture.krusty_refuses(
            "main",
            &sources,
            &android_classpath(),
            &parcelize_switches()
        ),
        "<fixture>/main/src/Raw.kt:10:5: error: krusty's Parcelize compiler plugin does not \
         support a property type with a Parcelize annotation yet; compile this module with \
         kotlinc\n\
         krusty: 1 error(s)\n"
    );
}
