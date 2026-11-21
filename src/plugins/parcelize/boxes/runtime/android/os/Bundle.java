package android.os;

public class Bundle extends BaseBundle implements Parcelable {
    public Bundle() {}

    public void putParcelable(String key, Parcelable value) { map.put(key, value); }
    public <T extends Parcelable> T getParcelable(String key) { return (T) map.get(key); }

    public int describeContents() { return 0; }

    public void writeToParcel(Parcel dest, int flags) { dest.writeMap(map); }

    public static final Creator<Bundle> CREATOR = new Creator<Bundle>() {
        public Bundle createFromParcel(Parcel source) {
            Bundle bundle = new Bundle();
            bundle.map.putAll(source.readMapValues());
            return bundle;
        }

        public Bundle[] newArray(int size) { return new Bundle[size]; }
    };
}
