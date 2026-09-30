//! Member overload identity is the published Kotlin signature. The return type is not part of it,
//! so two members that differ only by return are conflicting overloads. `vararg` and an explicit
//! array are different Kotlin signatures; they become one JVM method only when erasure, return
//! included, produces one descriptor. That collision is reported by the backend.

use super::common;

fn reject(src: &str) {
    common::assert_errors_match_kotlinc(&[("Main.kt", src)], &[]);
}

fn accept(src: &str) -> Vec<String> {
    common::front_end_diagnostics_files_with_stdlib(&[src])
}

#[test]
fn hidden_unit_array_overload_forwards_to_the_vararg() {
    let src = "class Element\n\
        class Payload\n\
        class Printer {\n\
        \x20   fun print(vararg objects: Element?): Payload = Payload()\n\
        \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
        \x20   fun print(objects: Array<Element?>) { print(*objects) }\n\
        }\n\
        fun box(): String {\n\
        \x20   val payload: Payload = Printer().print(Element(), Element())\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(accept(src), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(src, "HiddenUnitArray");
    let out = common::compile_lib("hidden-unit-array", src).expect("class files");
    let disassembly = common::javap(&[
        "-p",
        "-s",
        "-v",
        &out.join("Printer.class").to_string_lossy(),
    ])
    .expect("javap");
    let lines: Vec<&str> = disassembly.lines().map(str::trim).collect();
    let print_descriptors: Vec<&str> = lines
        .windows(2)
        .filter_map(|pair| {
            let descriptor = pair[1].strip_prefix("descriptor: ")?;
            let declaration = pair[0].trim_end_matches(';');
            let name = declaration.split('(').next()?.split_whitespace().last()?;
            (name == "print").then_some(descriptor)
        })
        .collect();
    assert_eq!(
        print_descriptors,
        ["([LElement;)LPayload;", "([LElement;)V"],
        "both exact overloads must be emitted: {disassembly}"
    );
    let hidden_method = disassembly
        .split("\n\n")
        .find(|block| {
            block
                .lines()
                .map(str::trim)
                .any(|line| line == "descriptor: ([LElement;)V")
        })
        .expect("hidden array overload block");
    assert!(
        hidden_method
            .lines()
            .map(str::trim)
            .any(|line| line == "flags: (0x1011) ACC_PUBLIC, ACC_FINAL, ACC_SYNTHETIC"),
        "the exact hidden array overload must be synthetic: {hidden_method}"
    );
}

#[test]
fn array_and_vararg_with_different_returns_are_distinct_methods() {
    let src = "class Element\n\
        class Payload\n\
        class Bucket<T>\n\
        class Printer {\n\
        \x20   fun print(vararg objects: Element?): Payload = Payload()\n\
        \x20   fun print(objects: Array<Element?>): Bucket<Element> = Bucket()\n\
        }\n\
        fun box(): String {\n\
        \x20   val printer = Printer()\n\
        \x20   val payload: Payload = printer.print(Element(), Element())\n\
        \x20   val bucket: Bucket<Element> = printer.print(arrayOf<Element?>(Element()))\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(accept(src), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(src, "ArrayVarargReturns");
}

#[test]
fn array_and_vararg_with_the_same_return_are_a_platform_clash() {
    let src = "class Element\n\
        class Payload\n\
        class Sink {\n\
        \x20   fun take(values: Array<Element>): Payload = Payload()\n\
        \x20   fun take(vararg values: Element): Payload = Payload()\n\
        }\n";
    reject(src);
}

#[test]
fn return_type_alone_does_not_overload_a_member() {
    let src = "class Element\n\
        class Payload\n\
        class C {\n\
        \x20   fun f(): Element = Element()\n\
        \x20   fun f(): Payload = Payload()\n\
        }\n";
    reject(src);
}

#[test]
fn alpha_equivalent_function_type_parameters_still_clash() {
    let src = "class Element\n\
        class Payload\n\
        class C {\n\
        \x20   fun <T> f(x: T): Element = Element()\n\
        \x20   fun <U> f(x: U): Payload = Payload()\n\
        }\n";
    reject(src);
}

#[test]
fn generic_arguments_with_different_returns_are_distinct_overloads() {
    let src = "class Element\n\
        class Payload\n\
        class Bucket<T>\n\
        class C {\n\
        \x20   fun f(x: Bucket<Element>): Element = Element()\n\
        \x20   fun f(x: Bucket<Payload>): Payload = Payload()\n\
        }\n\
        fun box(): String {\n\
        \x20   val c = C()\n\
        \x20   val element: Element = c.f(Bucket<Element>())\n\
        \x20   val payload: Payload = c.f(Bucket<Payload>())\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(accept(src), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(src, "ListReturnOverload");
}

#[test]
fn nullability_with_different_returns_is_a_distinct_overload() {
    let src = "class Element\n\
        class Payload\n\
        class C {\n\
        \x20   fun g(x: Element): Element = x\n\
        \x20   fun g(x: Element?): Payload = Payload()\n\
        }\n\
        fun box(): String {\n\
        \x20   val c = C()\n\
        \x20   val element: Element = c.g(Element())\n\
        \x20   val payload: Payload = c.g(null)\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(accept(src), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(src, "NullabilityReturnOverload");
}

#[test]
fn unused_formals_of_the_same_arity_still_clash() {
    let src = "class Element\n\
        class Payload\n\
        class C {\n\
        \x20   fun <T> f(): Element = Element()\n\
        \x20   fun <U> f(): Payload = Payload()\n\
        }\n";
    reject(src);
}

#[test]
fn unused_formals_of_differing_arity_with_one_return_are_a_platform_clash() {
    let src = "class Payload\n\
        class C {\n\
        \x20   fun <T> f(): Payload = Payload()\n\
        \x20   fun <T, U> f(): Payload = Payload()\n\
        }\n";
    reject(src);
}

#[test]
fn unused_formals_of_differing_arity_select_by_explicit_type_arguments() {
    let src = "class Element\n\
        class Payload\n\
        class C {\n\
        \x20   fun <T> f(): Element = Element()\n\
        \x20   fun <T, U> f(): Payload = Payload()\n\
        }\n\
        fun box(): String {\n\
        \x20   val c = C()\n\
        \x20   val element: Element = c.f<Element>()\n\
        \x20   val payload: Payload = c.f<Element, Payload>()\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(accept(src), Vec::<String>::new());
    common::expect_box_same_as_kotlinc(src, "UnusedFormalArity");
}

#[test]
fn inferred_returns_that_erase_together_are_a_platform_clash() {
    let src = "class Element\n\
        class Payload\n\
        class Bucket<T>\n\
        class C {\n\
        \x20   fun f(x: Bucket<Element>) = Payload()\n\
        \x20   fun f(x: Bucket<Payload>): Payload = Payload()\n\
        }\n";
    reject(src);
}
