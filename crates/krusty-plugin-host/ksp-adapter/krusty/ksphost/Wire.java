package krusty.ksphost;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

// The frame and value encodings of the protocol (`src/ksp/protocol.rs`): big-endian integers,
// strings as a u32 byte length and UTF-8, lists as a u32 count and the elements.
final class Wire {
    static final int PROTOCOL = 9;

    static final byte HELLO = 0;
    static final byte LOAD = 1;
    static final byte LOADED = 2;
    static final byte FAILED = 3;
    static final byte UNSUPPORTED = 4;
    static final byte ROUND = 5;
    static final byte ROUND_DONE = 6;
    static final byte FINISH = 7;
    static final byte FINISHED = 8;
    static final byte SHUTDOWN = 9;
    static final byte CLASS_QUERY = 10;
    static final byte CLASS_ANSWER = 11;
    static final byte NAME_QUERY = 12;
    static final byte NAME_ANSWER = 13;

    interface Body {
        void write(DataOutputStream out) throws IOException;
    }

    interface Element<T> {
        T read(DataInputStream in) throws IOException;
    }

    private Wire() {}

    static DataInputStream readFrame(DataInputStream frames) throws IOException {
        byte[] bytes = new byte[frames.readInt()];
        frames.readFully(bytes);
        return new DataInputStream(new ByteArrayInputStream(bytes));
    }

    static void send(DataOutputStream frames, Body body) throws IOException {
        ByteArrayOutputStream payload = new ByteArrayOutputStream();
        DataOutputStream out = new DataOutputStream(payload);
        body.write(out);
        out.flush();
        frames.writeInt(payload.size());
        payload.writeTo(frames);
        frames.flush();
    }

    static String readString(DataInputStream in) throws IOException {
        byte[] bytes = new byte[in.readInt()];
        in.readFully(bytes);
        return new String(bytes, StandardCharsets.UTF_8);
    }

    static boolean readBoolean(DataInputStream in) throws IOException {
        return in.readByte() != 0;
    }

    static <T> List<T> readList(DataInputStream in, Element<T> element) throws IOException {
        int count = in.readInt();
        List<T> list = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            list.add(element.read(in));
        }
        return list;
    }

    static void writeString(DataOutputStream out, String value) throws IOException {
        byte[] bytes = value.getBytes(StandardCharsets.UTF_8);
        out.writeInt(bytes.length);
        out.write(bytes);
    }
}
