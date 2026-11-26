package android.os;

public class PersistableBundle extends BaseBundle implements Parcelable {
    public int describeContents() { return 0; }

    public void writeToParcel(Parcel dest, int flags) { dest.writeMap(map); }

    public static final Creator<PersistableBundle> CREATOR = new Creator<PersistableBundle>() {
        public PersistableBundle createFromParcel(Parcel source) {
            PersistableBundle bundle = new PersistableBundle();
            bundle.map.putAll(source.readMapValues());
            return bundle;
        }

        public PersistableBundle[] newArray(int size) { return new PersistableBundle[size]; }
    };
}
