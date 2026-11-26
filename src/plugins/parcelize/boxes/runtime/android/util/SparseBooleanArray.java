package android.util;

import java.util.TreeMap;

public class SparseBooleanArray {
    private final TreeMap<Integer, Boolean> map = new TreeMap<>();

    public void put(int key, boolean value) { map.put(key, value); }
    public void append(int key, boolean value) { map.put(key, value); }
    public boolean get(int key) { return map.getOrDefault(key, false); }
    public boolean get(int key, boolean fallback) { return map.getOrDefault(key, fallback); }
    public void delete(int key) { map.remove(key); }
    public int size() { return map.size(); }
    public int keyAt(int index) { return (Integer) map.keySet().toArray()[index]; }
    public boolean valueAt(int index) { return (Boolean) map.values().toArray()[index]; }

    @Override
    public boolean equals(Object other) {
        return other instanceof SparseBooleanArray && map.equals(((SparseBooleanArray) other).map);
    }

    @Override
    public int hashCode() { return map.hashCode(); }
}
