//! A local class that writes or reads a private member-extension property of its enclosing class
//! reaches the accessor through `access$<name>`. The accessor is a private instance method, and
//! the local class is a separate class file.

use super::common;

#[test]
fn local_class_writes_private_member_extension() {
    // `extensionProperties/accessorForPrivateSetter.kt`
    common::expect_box_ok_with_stdlib(
        "class A {\n\
             var result = \"Fail\"\n\
             private var Int.foo: String\n\
                 get() = result\n\
                 private set(value) { result = value }\n\
             fun run(): String {\n\
                 class O {\n\
                     fun run() { 42.foo = \"OK\" }\n\
                 }\n\
                 O().run()\n\
                 return (-42).foo\n\
             }\n\
         }\n\
         fun box() = A().run()\n",
        "accessorForPrivateSetter",
    );
}

#[test]
fn local_class_reads_private_member_extension() {
    common::expect_box_ok_with_stdlib(
        "class B {\n\
             private val Int.foo: String\n\
                 get() = if (this == 7) \"OK\" else \"fail\"\n\
             fun run(): String {\n\
                 class O {\n\
                     fun run() = 7.foo\n\
                 }\n\
                 return O().run()\n\
             }\n\
         }\n\
         fun box() = B().run()\n",
        "privateMemberExtensionRead",
    );
}
