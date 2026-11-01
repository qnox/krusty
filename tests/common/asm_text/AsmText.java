import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.PrintStream;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.util.regex.PatternSyntaxException;

import org.jetbrains.org.objectweb.asm.ClassReader;
import org.jetbrains.org.objectweb.asm.ClassVisitor;
import org.jetbrains.org.objectweb.asm.Opcodes;
import org.jetbrains.org.objectweb.asm.util.Textifier;
import org.jetbrains.org.objectweb.asm.util.TraceClassVisitor;

/**
 * A persistent server over the ASM copy the reference compiler distribution ships, for the bytecode
 * corpora's oracles. Every request is answered in order on standard output; standard output is
 * reserved for the protocol.
 *
 * <ul>
 *   <li>{@code T n (len bytes)*}: for each class file, the source file it names (empty when it
 *       names none) and the class as ASM's {@link Textifier} prints it.</li>
 *   <li>{@code C text n needle*}: for each needle, how many times {@code Pattern.compile("(" +
 *       needle + ")")} is found in {@code text}, as {@code StringUtil.findMatches} counts it; -1 for
 *       a needle that is not a valid pattern.</li>
 * </ul>
 *
 * Strings and byte arrays travel as a big-endian int length and their (UTF-8) bytes. A response is
 * a status int (0 = ok, 1 = error followed by its message) and the answers.
 */
public final class AsmText {
    public static void main(String[] args) throws Exception {
        DataInputStream in = new DataInputStream(new BufferedInputStream(System.in, 1 << 16));
        DataOutputStream out = new DataOutputStream(new BufferedOutputStream(System.out, 1 << 16));
        System.setOut(new PrintStream(System.err, true, "UTF-8"));
        while (true) {
            int command;
            try {
                command = in.readUnsignedByte();
            } catch (EOFException end) {
                return;
            }
            switch (command) {
                case 'T': textify(in, out); break;
                case 'C': count(in, out); break;
                default: throw new IllegalStateException("unknown command " + command);
            }
            out.flush();
        }
    }

    private static void textify(DataInputStream in, DataOutputStream out) throws Exception {
        int n = in.readInt();
        byte[][] classes = new byte[n][];
        for (int i = 0; i < n; i++) classes[i] = bytes(in);
        String[] sources = new String[n];
        String[] texts = new String[n];
        try {
            for (int i = 0; i < n; i++) {
                StringWriter text = new StringWriter();
                String[] source = {""};
                ClassVisitor trace = new TraceClassVisitor(null, new Textifier(), new PrintWriter(text));
                new ClassReader(classes[i]).accept(new ClassVisitor(Opcodes.API_VERSION, trace) {
                    @Override
                    public void visitSource(String file, String debug) {
                        if (file != null) source[0] = file;
                        super.visitSource(file, debug);
                    }
                }, 0);
                sources[i] = source[0];
                texts[i] = text.toString();
            }
        } catch (RuntimeException failure) {
            error(out, failure);
            return;
        }
        out.writeInt(0);
        for (int i = 0; i < n; i++) {
            string(out, sources[i]);
            string(out, texts[i]);
        }
    }

    private static void count(DataInputStream in, DataOutputStream out) throws Exception {
        String text = new String(bytes(in), StandardCharsets.UTF_8);
        int n = in.readInt();
        int[] counts = new int[n];
        for (int i = 0; i < n; i++) {
            String needle = new String(bytes(in), StandardCharsets.UTF_8);
            try {
                Matcher matcher = Pattern.compile("(" + needle + ")").matcher(text);
                int found = 0;
                while (matcher.find()) found++;
                counts[i] = found;
            } catch (PatternSyntaxException invalid) {
                counts[i] = -1;
            }
        }
        out.writeInt(0);
        for (int found : counts) out.writeInt(found);
    }

    private static byte[] bytes(DataInputStream in) throws Exception {
        byte[] data = new byte[in.readInt()];
        in.readFully(data);
        return data;
    }

    private static void string(DataOutputStream out, String text) throws Exception {
        byte[] data = text.getBytes(StandardCharsets.UTF_8);
        out.writeInt(data.length);
        out.write(data);
    }

    private static void error(DataOutputStream out, Throwable failure) throws Exception {
        StringWriter trace = new StringWriter();
        failure.printStackTrace(new PrintWriter(trace));
        out.writeInt(1);
        string(out, trace.toString());
    }
}
