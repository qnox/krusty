package krusty.ksphost;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.PrintWriter;
import java.io.StringWriter;

// The KSP host's entry point: it speaks the protocol on stdin and stdout, and gives stdout's
// System.out to stderr so that what a processor prints cannot corrupt a frame.
public final class Host {
    private Host() {}

    public static void main(String[] args) throws IOException {
        DataInputStream in = new DataInputStream(new BufferedInputStream(System.in));
        DataOutputStream out =
                new DataOutputStream(new BufferedOutputStream(new FileOutputStream(FileDescriptor.out)));
        System.setOut(System.err);
        Wire.send(out, w -> {
            w.writeByte(Wire.HELLO);
            w.writeInt(Wire.PROTOCOL);
        });
        Session session = null;
        while (true) {
            DataInputStream request = Wire.readFrame(in);
            byte tag = request.readByte();
            try {
                switch (tag) {
                    case Wire.LOAD: {
                        if (session != null) {
                            session.close();
                        }
                        session = Session.load(request);
                        Session loaded = session;
                        Wire.send(out, w -> {
                            w.writeByte(Wire.LOADED);
                            w.writeInt(loaded.providers.size());
                            for (String provider : loaded.providers) {
                                Wire.writeString(w, provider);
                            }
                        });
                        break;
                    }
                    case Wire.ROUND: {
                        Session.Outcome outcome = requireSession(session).round(Symbols.read(request));
                        sendOutcome(out, Wire.ROUND_DONE, outcome);
                        break;
                    }
                    case Wire.FINISH: {
                        Session.Outcome outcome = requireSession(session).finish();
                        sendOutcome(out, Wire.FINISHED, outcome);
                        break;
                    }
                    case Wire.SHUTDOWN:
                        if (session != null) {
                            session.close();
                        }
                        return;
                    default:
                        throw new IOException("unknown request " + tag);
                }
            } catch (Throwable error) {
                NotImplemented missing = NotImplemented.in(error);
                if (missing != null) {
                    Wire.send(out, w -> {
                        w.writeByte(Wire.UNSUPPORTED);
                        Wire.writeString(w, missing.member);
                    });
                } else {
                    StringWriter trace = new StringWriter();
                    error.printStackTrace(new PrintWriter(trace));
                    Wire.send(out, w -> {
                        w.writeByte(Wire.FAILED);
                        Wire.writeString(w, trace.toString());
                    });
                }
            }
        }
    }

    private static Session requireSession(Session session) throws IOException {
        if (session == null) {
            throw new IOException("no processors are loaded");
        }
        return session;
    }

    private static void sendOutcome(DataOutputStream out, byte tag, Session.Outcome outcome)
            throws IOException {
        Wire.send(out, w -> {
            w.writeByte(tag);
            w.writeInt(outcome.generated.size());
            for (CodeGen.Generated file : outcome.generated) {
                w.writeByte(file.kind);
                Wire.writeString(w, file.path);
            }
            w.writeInt(outcome.log.size());
            for (String line : outcome.log) {
                Wire.writeString(w, line);
            }
            w.writeByte(outcome.hasError ? 1 : 0);
        });
    }
}
