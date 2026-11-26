package android.util;

import java.util.TreeMap;

public class SparseArray<E> {
    private final TreeMap<Integer, E> map = new TreeMap<>();

    public void put(int key, E value) { map.put(key, value); }
    public void append(int key, E value) { map.put(key, value); }
    public E get(int key) { return map.get(key); }
    public E get(int key, E fallback) { return map.containsKey(key) ? map.get(key) : fallback; }
    public void remove(int key) { map.remove(key); }
    public void delete(int key) { map.remove(key); }
    public int size() { return map.size(); }
    public int keyAt(int index) { return (Integer) map.keySet().toArray()[index]; }
    @SuppressWarnings("unchecked")
    public E valueAt(int index) { return (E) map.values().toArray()[index]; }
    public int indexOfKey(int key) { return new java.util.ArrayList<>(map.keySet()).indexOf(key); }

    @Override
    public boolean equals(Object other) {
        return other instanceof SparseArray && map.equals(((SparseArray<?>) other).map);
    }

    @Override
    public int hashCode() { return map.hashCode(); }
}
