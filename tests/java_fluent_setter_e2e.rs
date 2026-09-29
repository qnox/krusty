//! A Java getter/setter pair is a Kotlin `var` whatever `setX` returns. The assignment calls
//! that setter and discards the result: a fluent receiver, an unrelated return, an inherited
//! setter whose return is the superclass, and `void` all store the value.

use super::common;

#[test]
fn java_setter_is_a_var_whatever_it_returns() {
    let fluent = "package fixtures;\n\
        public class Fluent {\n\
        \x20 private String name = \"\";\n\
        \x20 private int count;\n\
        \x20 private long serial;\n\
        \x20 private String label = \"\";\n\
        \x20 public String getName() { return name; }\n\
        \x20 public Fluent setName(String name) { this.name = name; return this; }\n\
        \x20 public int getCount() { return count; }\n\
        \x20 public String setCount(int count) { this.count = count; return \"set\"; }\n\
        \x20 public long getSerial() { return serial; }\n\
        \x20 public long setSerial(long serial) { this.serial = serial; return serial; }\n\
        \x20 public String getLabel() { return label; }\n\
        \x20 public void setLabel(String label) { this.label = label; }\n\
        }\n";
    let child = "package fixtures;\n\
        public class Child extends Fluent {\n\
        \x20 private String tag = \"\";\n\
        \x20 public String getTag() { return tag; }\n\
        \x20 public Child setTag(String tag) { this.tag = tag; return this; }\n\
        }\n";
    let use_src = "import fixtures.Child\n\
        import fixtures.Fluent\n\
        import java.nio.file.attribute.FileTime\n\
        import java.util.zip.ZipEntry\n\
        fun box(): String {\n\
        \x20 val f = Fluent()\n\
        \x20 f.name = \"a\"\n\
        \x20 f.count = 3\n\
        \x20 f.serial = 9L\n\
        \x20 f.label = \"b\"\n\
        \x20 if (f.name != \"a\") return \"name\"\n\
        \x20 if (f.count != 3) return \"count\"\n\
        \x20 if (f.serial != 9L) return \"serial\"\n\
        \x20 if (f.label != \"b\") return \"label\"\n\
        \x20 val c = Child()\n\
        \x20 c.name = \"c\"\n\
        \x20 c.tag = \"t\"\n\
        \x20 if (c.name != \"c\") return \"child-name\"\n\
        \x20 if (c.tag != \"t\") return \"tag\"\n\
        \x20 val entry = ZipEntry(\"z\")\n\
        \x20 val stamp = FileTime.fromMillis(0)\n\
        \x20 entry.creationTime = stamp\n\
        \x20 entry.lastModifiedTime = stamp\n\
        \x20 entry.lastAccessTime = stamp\n\
        \x20 if (entry.creationTime.toMillis() != 0L) return \"created\"\n\
        \x20 if (entry.lastModifiedTime.toMillis() != 0L) return \"modified\"\n\
        \x20 if (entry.lastAccessTime.toMillis() != 0L) return \"accessed\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let out = common::java_interop_box(
        "java_fluent_setter",
        &[("Fluent.java", fluent), ("Child.java", child)],
        use_src,
    );
    assert_eq!(out, "OK");
}
