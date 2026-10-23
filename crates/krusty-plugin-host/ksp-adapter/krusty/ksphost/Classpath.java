package krusty.ksphost;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.util.HashMap;
import java.util.Map;

// The classifiers of the module's classpath. The first time a processor reaches one, the host asks
// krusty for it by name in the middle of the request it is working on, and keeps the answer for
// the module's later rounds: a library does not change while processors run.
final class Classpath {
    private final DataInputStream in;
    private final DataOutputStream out;
    // Each classifier asked for, null for a name the classpath does not declare.
    private final Map<String, LibraryClass> classes = new HashMap<>();

    Classpath(DataInputStream in, DataOutputStream out) {
        this.in = in;
        this.out = out;
    }

    // The classifier `qualifiedName` names, or null when the classpath declares none.
    LibraryClass classNamed(String qualifiedName) {
        if (classes.containsKey(qualifiedName)) {
            return classes.get(qualifiedName);
        }
        LibraryClass declaration;
        try {
            declaration = ask(qualifiedName);
        } catch (IOException error) {
            throw new UncheckedIOException(error);
        }
        classes.put(qualifiedName, declaration);
        return declaration;
    }

    private LibraryClass ask(String qualifiedName) throws IOException {
        Wire.send(out, w -> {
            w.writeByte(Wire.CLASS_QUERY);
            Wire.writeString(w, qualifiedName);
        });
        DataInputStream answer = Wire.readFrame(in);
        byte tag = answer.readByte();
        if (tag != Wire.CLASS_ANSWER) {
            throw new IOException("expected the answer for " + qualifiedName + ", got message " + tag);
        }
        return Wire.readBoolean(answer) ? Symbols.readLibraryClass(answer, this) : null;
    }
}
