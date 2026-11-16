package krusty.tooling;

import java.io.PrintStream;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** One JSON object per line. The language server owns the schema. */
final class Protocol {
    /**
     * The stream the language server reads. Maven's resolver prints
     * {@code [WARNING] The POM for ... is invalid} on {@code System.out}. That line is not a
     * response; reading it as one stops the resolve and leaves every later jar unrequested.
     * {@link #capture()} keeps this stream for responses and sends those warnings to stderr.
     */
    private static PrintStream protocol = System.out;
    private static boolean captured;

    private final String text;
    private int index;

    static void capture() {
        if (captured) {
            return;
        }
        captured = true;
        protocol = System.out;
        System.setOut(System.err);
    }

    private Protocol(String text) {
        this.text = text;
    }

    static Map<String, Object> object(String text) {
        Protocol parser = new Protocol(text);
        parser.skip();
        Object value = parser.value();
        if (!(value instanceof Map<?, ?> map)) {
            throw new IllegalArgumentException("expected an object");
        }
        Map<String, Object> object = new LinkedHashMap<>();
        for (Map.Entry<?, ?> entry : map.entrySet()) {
            object.put(String.valueOf(entry.getKey()), entry.getValue());
        }
        return object;
    }

    static String text(Map<String, Object> object, String key) {
        Object value = object.get(key);
        if (value == null) {
            return "";
        }
        if (value instanceof String text) {
            return text;
        }
        throw new IllegalArgumentException(key);
    }

    static boolean flag(Map<String, Object> object, String key) {
        Object value = object.get(key);
        if (value instanceof Boolean flag) {
            return flag;
        }
        return false;
    }

    static List<String> strings(Map<String, Object> object, String key) {
        List<String> values = new ArrayList<>();
        for (Object value : list(object, key)) {
            if (!(value instanceof String text)) {
                throw new IllegalArgumentException(key);
            }
            values.add(text);
        }
        return values;
    }

    static List<Map<String, Object>> objects(Map<String, Object> object, String key) {
        List<Map<String, Object>> values = new ArrayList<>();
        for (Object value : list(object, key)) {
            if (!(value instanceof Map<?, ?> map)) {
                throw new IllegalArgumentException(key);
            }
            Map<String, Object> item = new LinkedHashMap<>();
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                item.put(String.valueOf(entry.getKey()), entry.getValue());
            }
            values.add(item);
        }
        return values;
    }

    static void respond(int status, String stdout, String stderr) {
        StringBuilder builder = new StringBuilder();
        builder.append("{\"status\":").append(status).append(",\"stdout\":");
        quote(builder, stdout);
        builder.append(",\"stderr\":");
        quote(builder, stderr);
        builder.append("}\n");
        protocol.print(builder);
        protocol.flush();
    }

    private static List<?> list(Map<String, Object> object, String key) {
        Object value = object.get(key);
        if (value == null) {
            return List.of();
        }
        if (value instanceof List<?> list) {
            return list;
        }
        throw new IllegalArgumentException(key);
    }

    private static void quote(StringBuilder builder, String value) {
        builder.append('"');
        for (int i = 0; i < value.length(); i++) {
            char character = value.charAt(i);
            switch (character) {
                case '"' -> builder.append("\\\"");
                case '\\' -> builder.append("\\\\");
                case '\n' -> builder.append("\\n");
                case '\r' -> builder.append("\\r");
                case '\t' -> builder.append("\\t");
                default -> {
                    if (character < 0x20) {
                        builder.append(String.format("\\u%04x", (int) character));
                    } else {
                        builder.append(character);
                    }
                }
            }
        }
        builder.append('"');
    }

    private Object value() {
        skip();
        if (index >= text.length()) {
            throw new IllegalArgumentException("truncated JSON");
        }
        char character = text.charAt(index);
        if (character == '{') {
            return objectValue();
        }
        if (character == '[') {
            return arrayValue();
        }
        if (character == '"') {
            return stringValue();
        }
        if (text.startsWith("true", index)) {
            index += 4;
            return Boolean.TRUE;
        }
        if (text.startsWith("false", index)) {
            index += 5;
            return Boolean.FALSE;
        }
        if (text.startsWith("null", index)) {
            index += 4;
            return null;
        }
        return numberValue();
    }

    private Map<String, Object> objectValue() {
        index++;
        Map<String, Object> object = new LinkedHashMap<>();
        skip();
        if (consume('}')) {
            return object;
        }
        while (true) {
            skip();
            String key = stringValue();
            skip();
            if (!consume(':')) {
                throw new IllegalArgumentException("expected ':'");
            }
            object.put(key, value());
            skip();
            if (consume('}')) {
                return object;
            }
            if (!consume(',')) {
                throw new IllegalArgumentException("expected ','");
            }
        }
    }

    private List<Object> arrayValue() {
        index++;
        List<Object> values = new ArrayList<>();
        skip();
        if (consume(']')) {
            return values;
        }
        while (true) {
            values.add(value());
            skip();
            if (consume(']')) {
                return values;
            }
            if (!consume(',')) {
                throw new IllegalArgumentException("expected ','");
            }
        }
    }

    private String stringValue() {
        if (!consume('"')) {
            throw new IllegalArgumentException("expected a string");
        }
        StringBuilder builder = new StringBuilder();
        while (index < text.length()) {
            char character = text.charAt(index++);
            if (character == '"') {
                return builder.toString();
            }
            if (character != '\\') {
                builder.append(character);
                continue;
            }
            if (index >= text.length()) {
                throw new IllegalArgumentException("truncated escape");
            }
            char escaped = text.charAt(index++);
            switch (escaped) {
                case '"', '\\', '/' -> builder.append(escaped);
                case 'b' -> builder.append('\b');
                case 'f' -> builder.append('\f');
                case 'n' -> builder.append('\n');
                case 'r' -> builder.append('\r');
                case 't' -> builder.append('\t');
                case 'u' -> {
                    if (index + 4 > text.length()) {
                        throw new IllegalArgumentException("truncated unicode escape");
                    }
                    int code = Integer.parseInt(text.substring(index, index + 4), 16);
                    builder.append((char) code);
                    index += 4;
                }
                default -> throw new IllegalArgumentException("bad escape");
            }
        }
        throw new IllegalArgumentException("unterminated string");
    }

    private String numberValue() {
        int start = index;
        while (index < text.length()) {
            char character = text.charAt(index);
            if ((character >= '0' && character <= '9') || character == '-' || character == '+'
                || character == '.' || character == 'e' || character == 'E') {
                index++;
            } else {
                break;
            }
        }
        if (start == index) {
            throw new IllegalArgumentException("expected a value");
        }
        return text.substring(start, index);
    }

    private void skip() {
        while (index < text.length() && Character.isWhitespace(text.charAt(index))) {
            index++;
        }
    }

    private boolean consume(char expected) {
        if (index < text.length() && text.charAt(index) == expected) {
            index++;
            return true;
        }
        return false;
    }
}
