package android.os;

import android.util.SparseArray;
import android.util.SparseBooleanArray;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.ObjectInputStream;
import java.io.ObjectOutputStream;
import java.io.Serializable;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** A parcel that stores each written value in order and reads them back in the same order. */
public final class Parcel {
    private ArrayList<Object> values = new ArrayList<>();
    private int position;

    public static Parcel obtain() { return new Parcel(); }
    public void recycle() { values = new ArrayList<>(); position = 0; }
    public void setDataPosition(int position) { this.position = position; }
    public int dataPosition() { return position; }
    public int dataSize() { return values.size(); }
    public int dataAvail() { return values.size() - position; }

    public byte[] marshall() {
        try {
            ByteArrayOutputStream bytes = new ByteArrayOutputStream();
            try (ObjectOutputStream out = new ObjectOutputStream(bytes)) {
                out.writeObject(values);
            }
            return bytes.toByteArray();
        } catch (java.io.IOException error) {
            throw new RuntimeException(error);
        }
    }

    @SuppressWarnings("unchecked")
    public void unmarshall(byte[] data, int offset, int length) {
        try (ObjectInputStream in = new ObjectInputStream(new ByteArrayInputStream(data, offset, length))) {
            values = (ArrayList<Object>) in.readObject();
            position = 0;
        } catch (java.io.IOException | ClassNotFoundException error) {
            throw new RuntimeException(error);
        }
    }

    private Object next() { return values.get(position++); }
    private void put(Object value) { values.add(value); }

    public void writeString(String value) { put(value); }
    public String readString() { return (String) next(); }
    public void writeString8(String value) { put(value); }
    public String readString8() { return (String) next(); }
    public void writeString16(String value) { put(value); }
    public String readString16() { return (String) next(); }
    public void writeCharSequence(CharSequence value) { put(value == null ? null : value.toString()); }
    public CharSequence readCharSequence() { return (CharSequence) next(); }
    public void writeByte(byte value) { put(value); }
    public byte readByte() { return (Byte) next(); }
    public void writeInt(int value) { put(value); }
    public int readInt() { return (Integer) next(); }
    public void writeLong(long value) { put(value); }
    public long readLong() { return (Long) next(); }
    public void writeFloat(float value) { put(value); }
    public float readFloat() { return (Float) next(); }
    public void writeDouble(double value) { put(value); }
    public double readDouble() { return (Double) next(); }

    public void writeByteArray(byte[] value) { put(value == null ? null : value.clone()); }
    public byte[] createByteArray() { return (byte[]) next(); }
    public void readByteArray(byte[] into) { System.arraycopy((byte[]) next(), 0, into, 0, into.length); }
    public void writeIntArray(int[] value) { put(value == null ? null : value.clone()); }
    public int[] createIntArray() { return (int[]) next(); }
    public void writeLongArray(long[] value) { put(value == null ? null : value.clone()); }
    public long[] createLongArray() { return (long[]) next(); }
    public void writeFloatArray(float[] value) { put(value == null ? null : value.clone()); }
    public float[] createFloatArray() { return (float[]) next(); }
    public void writeDoubleArray(double[] value) { put(value == null ? null : value.clone()); }
    public double[] createDoubleArray() { return (double[]) next(); }
    public void writeBooleanArray(boolean[] value) { put(value == null ? null : value.clone()); }
    public boolean[] createBooleanArray() { return (boolean[]) next(); }
    public void writeCharArray(char[] value) { put(value == null ? null : value.clone()); }
    public char[] createCharArray() { return (char[]) next(); }
    public void writeStringArray(String[] value) { put(value == null ? null : value.clone()); }
    public String[] createStringArray() { return (String[]) next(); }
    public void writeStringList(List<String> value) { put(value == null ? null : new ArrayList<>(value)); }
    @SuppressWarnings("unchecked")
    public ArrayList<String> createStringArrayList() { return (ArrayList<String>) next(); }

    public void writeSerializable(Serializable value) { put(value); }
    public Serializable readSerializable() { return (Serializable) next(); }

    public void writeParcelable(Parcelable value, int flags) {
        if (value == null) {
            writeString(null);
            return;
        }
        writeString(value.getClass().getName());
        value.writeToParcel(this, flags);
    }

    public <T extends Parcelable> T readParcelable(ClassLoader loader) {
        String name = readString();
        return name == null ? null : readCreated(name, loader);
    }

    public <T extends Parcelable> T readParcelable(ClassLoader loader, Class<T> type) {
        return readParcelable(loader);
    }

    @SuppressWarnings("unchecked")
    private <T> T readCreated(String name, ClassLoader loader) {
        try {
            Class<?> type = Class.forName(name, false, loader != null ? loader : getClass().getClassLoader());
            Field creator = type.getField("CREATOR");
            return (T) ((Parcelable.Creator<?>) creator.get(null)).createFromParcel(this);
        } catch (ReflectiveOperationException error) {
            throw new RuntimeException(error);
        }
    }

    public <T extends Parcelable> void writeTypedObject(T value, int flags) {
        writeInt(value == null ? 0 : 1);
        if (value != null) value.writeToParcel(this, flags);
    }

    public <T> T readTypedObject(Parcelable.Creator<T> creator) {
        return readInt() == 0 ? null : creator.createFromParcel(this);
    }

    public <T extends Parcelable> void writeTypedArray(T[] value, int flags) {
        if (value == null) {
            writeInt(-1);
            return;
        }
        writeInt(value.length);
        for (T element : value) writeTypedObject(element, flags);
    }

    public <T> T[] createTypedArray(Parcelable.Creator<T> creator) {
        int size = readInt();
        if (size < 0) return null;
        T[] array = creator.newArray(size);
        for (int index = 0; index < size; index++) array[index] = readTypedObject(creator);
        return array;
    }

    public <T extends Parcelable> void writeTypedList(List<T> value) {
        if (value == null) {
            writeInt(-1);
            return;
        }
        writeInt(value.size());
        for (T element : value) writeTypedObject(element, 0);
    }

    public <T> ArrayList<T> createTypedArrayList(Parcelable.Creator<T> creator) {
        int size = readInt();
        if (size < 0) return null;
        ArrayList<T> list = new ArrayList<>(size);
        for (int index = 0; index < size; index++) list.add(readTypedObject(creator));
        return list;
    }

    public <T> void readTypedList(List<T> into, Parcelable.Creator<T> creator) {
        into.clear();
        ArrayList<T> read = createTypedArrayList(creator);
        if (read != null) into.addAll(read);
    }

    public void writeBundle(Bundle value) { writeParcelable(value, 0); }
    public Bundle readBundle() { return readParcelable(null); }
    public Bundle readBundle(ClassLoader loader) { return readParcelable(loader); }
    public void writePersistableBundle(PersistableBundle value) { writeParcelable(value, 0); }
    public PersistableBundle readPersistableBundle() { return readParcelable(null); }

    void writeMap(Map<String, Object> map) { put(new LinkedHashMap<>(map)); }

    @SuppressWarnings("unchecked")
    Map<String, Object> readMapValues() { return (Map<String, Object>) next(); }

    public void writeMap(Map map, int unused) { put(map == null ? null : new HashMap<Object, Object>(map)); }
    @SuppressWarnings("unchecked")
    public HashMap readHashMap(ClassLoader loader) { return (HashMap) next(); }
    public void writeList(List value) { put(value == null ? null : new ArrayList<Object>(value)); }
    @SuppressWarnings("unchecked")
    public ArrayList readArrayList(ClassLoader loader) { return (ArrayList) next(); }

    public void writeSparseArray(SparseArray<Object> value) { put(value); }
    @SuppressWarnings("unchecked")
    public <T> SparseArray<T> readSparseArray(ClassLoader loader) { return (SparseArray<T>) next(); }
    public void writeSparseBooleanArray(SparseBooleanArray value) { put(value); }
    public SparseBooleanArray readSparseBooleanArray() { return (SparseBooleanArray) next(); }
    public void writeStrongBinder(IBinder value) { put(value); }
    public IBinder readStrongBinder() { return (IBinder) next(); }

    public void writeValue(Object value) { put(value); }
    public Object readValue(ClassLoader loader) { return next(); }
}
