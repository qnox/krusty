package android.os;

import java.io.Serializable;
import java.util.LinkedHashMap;
import java.util.Set;

public class BaseBundle implements Serializable {
    final LinkedHashMap<String, Object> map = new LinkedHashMap<>();

    public int size() { return map.size(); }
    public boolean isEmpty() { return map.isEmpty(); }
    public boolean containsKey(String key) { return map.containsKey(key); }
    public Set<String> keySet() { return map.keySet(); }
    public Object get(String key) { return map.get(key); }
    public void remove(String key) { map.remove(key); }
    public void clear() { map.clear(); }

    public void putString(String key, String value) { map.put(key, value); }
    public String getString(String key) { return (String) map.get(key); }
    public void putInt(String key, int value) { map.put(key, value); }
    public int getInt(String key) { Object v = map.get(key); return v == null ? 0 : (Integer) v; }
    public void putLong(String key, long value) { map.put(key, value); }
    public long getLong(String key) { Object v = map.get(key); return v == null ? 0L : (Long) v; }
    public void putDouble(String key, double value) { map.put(key, value); }
    public double getDouble(String key) { Object v = map.get(key); return v == null ? 0.0 : (Double) v; }
    public void putBoolean(String key, boolean value) { map.put(key, value); }
    public boolean getBoolean(String key) { Object v = map.get(key); return v != null && (Boolean) v; }
    public void putStringArray(String key, String[] value) { map.put(key, value); }
    public String[] getStringArray(String key) { return (String[]) map.get(key); }

    @Override
    public boolean equals(Object other) {
        return other != null && other.getClass() == getClass() && map.equals(((BaseBundle) other).map);
    }

    @Override
    public int hashCode() { return map.hashCode(); }
}
