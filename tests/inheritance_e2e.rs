//! Class inheritance: `open class Base(args)` + `class Sub(...) : Base(args)` — the subclass
//! `extends` the base, its constructor calls `super(args)`, and it inherits the base's methods and
//! properties (open members are non-`final`). Compiled by krusty, run on a real JVM.

mod common;

#[test]
fn subclass_inherits_and_overrides() {
    let src = "open class Animal(val name: String) {\n  fun describe(): String = \"animal:\" + name\n}\nclass Dog(val tag: Int) : Animal(\"rex\") {\n  fun bark(): String = \"woof\"\n}\nfun box(): String {\n  val d = Dog(7)\n  if (d.bark() != \"woof\") return \"f1\"\n  if (d.describe() != \"animal:rex\") return \"f2\"\n  if (d.name != \"rex\") return \"f3\"\n  if (d.tag != 7) return \"f4\"\n  return \"OK\"\n}\n";
    common::assert_box_ok_with_stdlib(src, "H");
}
