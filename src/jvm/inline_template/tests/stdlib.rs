//! Decoding inline functions the Kotlin standard library compiles, read from the stdlib jar the
//! reference compiler ships.

use std::io::Read;

use super::render;
use crate::jvm::classreader;
use crate::jvm::inline_template::decode;
use crate::jvm::method_node::MethodNode;

/// The stdlib jar's entries, or `None` when the reference toolchain is not provisioned.
fn stdlib() -> Option<zip::ZipArchive<std::fs::File>> {
    let jar = crate::toolchain::stdlib_jar()?;
    Some(zip::ZipArchive::new(std::fs::File::open(jar).expect("open stdlib")).expect("zip"))
}

fn class_bytes(archive: &mut zip::ZipArchive<std::fs::File>, class: &str) -> Vec<u8> {
    let mut entry = archive
        .by_name(&format!("{class}.class"))
        .unwrap_or_else(|error| panic!("{class}: {error}"));
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read class");
    bytes
}

/// The method `name``desc` of `bytes`, read into a node.
fn read_method(bytes: &[u8], name: &str, desc: &str) -> MethodNode {
    let class = classreader::parse_class(bytes).expect("parse class");
    let access = class
        .methods
        .iter()
        .find(|method| method.name == name && method.descriptor == desc)
        .unwrap_or_else(|| panic!("no method {name}{desc}"))
        .access;
    let body = classreader::read_method_code(bytes, name, desc).expect("method code");
    MethodNode::read(access, name, desc, &body).expect("read method")
}

/// The rendered decoding of `class.name desc` from the stdlib, `None` without a stdlib.
fn decoded(class: &str, name: &str, desc: &str) -> Option<String> {
    let mut archive = stdlib()?;
    let bytes = class_bytes(&mut archive, class);
    let method = read_method(&bytes, name, desc);
    Some(render(&decode(class, &method).expect("decode")))
}

/// `UIntArray(size, init)`: the loop that fills an `int[]` through the boxed `init` lambda,
/// with the parameter null check, the unboxing `UInt.unbox-impl` and the value-class constructor
/// kept as exact calls.
#[test]
fn uint_array_constructor_with_init() {
    let Some(text) = decoded(
        "kotlin/UIntArrayKt",
        "UIntArray",
        "(ILkotlin/jvm/functions/Function1;)[I",
    ) else {
        return;
    };
    assert_eq!(
        text,
        "\
l0 = slot 0 Int parameter
l1 = slot 1 Reference parameter
l2 = slot 2 Int
l3 = slot 3 Reference
l4 = slot 4 Int
b0():
  [-] v0: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [-] v1: Reference(\"Ljava/lang/String;\") = Constant(String(\"init\"))
  [-] invoke Static kotlin/jvm/internal/Intrinsics.checkNotNullParameter(Ljava/lang/Object;Ljava/lang/String;)V (v0, v1)
  [75] v2: Int = Constant(Int(0))
  [75] store l2 v2
  [75] v3: Int = load l0
  [75] v4: Reference(\"[I\") = NewArray { element: \"I\", length: ValueId(3) }
  [75] store l3 v4
  [75] goto b1()
b1():
  [75] v5: Int = load l2
  [75] v6: Int = load l0
  [75] branch Ints { comparison: Ge, lhs: ValueId(5), rhs: ValueId(6) } ? b3() : b2()
b2():
  [75] v7: Int = load l2
  [75] store l4 v7
  [75] v8: Reference(\"[I\") = load l3
  [75] v9: Int = load l4
  [75] v10: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [75] v11: Int = load l4
  [75] v12: Reference(\"Ljava/lang/Integer;\") = invoke Static java/lang/Integer.valueOf(I)Ljava/lang/Integer; (v11)
  [75] v13: Reference(\"Ljava/lang/Object;\") = invoke Interface kotlin/jvm/functions/Function1.invoke(Ljava/lang/Object;)Ljava/lang/Object; interface (v10, v12)
  [75] v14: Reference(\"Lkotlin/UInt;\") = CheckCast { class: \"kotlin/UInt\", value: ValueId(13) }
  [75] v15: Int = invoke Virtual kotlin/UInt.unbox-impl()I (v14)
  [75] ArrayStore { element: Int, array: ValueId(8), index: ValueId(9), value: ValueId(15) }
  [75] IncrementLocal { local: LocalId(2), delta: 1 }
  [75] goto b1()
b3():
  [75] v16: Reference(\"[I\") = load l3
  [75] v17: Reference(\"[I\") = invoke Static kotlin/UIntArray.constructor-impl([I)[I (v16)
  [75] return v17
scope l0 size I At { block: BlockId(0), op: 0 }..End
scope l1 init Lkotlin/jvm/functions/Function1; At { block: BlockId(0), op: 0 }..End
"
    );
}

/// `repeat(times, action)`: a counted loop over the `index` local, back edge included, whose
/// popped lambda result leaves no operation.
#[test]
fn repeat_loop() {
    let Some(text) = decoded(
        "kotlin/StandardKt__StandardKt",
        "repeat",
        "(ILkotlin/jvm/functions/Function1;)V",
    ) else {
        return;
    };
    assert_eq!(
        text,
        "\
l0 = slot 0 Int parameter
l1 = slot 1 Reference parameter
l2 = slot 2 Int
b0():
  [-] v0: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [-] v1: Reference(\"Ljava/lang/String;\") = Constant(String(\"action\"))
  [-] invoke Static kotlin/jvm/internal/Intrinsics.checkNotNullParameter(Ljava/lang/Object;Ljava/lang/String;)V (v0, v1)
  [162] v2: Int = Constant(Int(0))
  [162] store l2 v2
  [162] goto b1()
b1():
  [162] v3: Int = load l2
  [162] v4: Int = load l0
  [162] branch Ints { comparison: Ge, lhs: ValueId(3), rhs: ValueId(4) } ? b3() : b2()
b2():
  [163] v5: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [163] v6: Int = load l2
  [163] v7: Reference(\"Ljava/lang/Integer;\") = invoke Static java/lang/Integer.valueOf(I)Ljava/lang/Integer; (v6)
  [163] v8: Reference(\"Ljava/lang/Object;\") = invoke Interface kotlin/jvm/functions/Function1.invoke(Ljava/lang/Object;)Ljava/lang/Object; interface (v5, v7)
  [162] IncrementLocal { local: LocalId(2), delta: 1 }
  [162] goto b1()
b3():
  [165] return
scope l2 index I At { block: BlockId(1), op: 0 }..At { block: BlockId(3), op: 0 }
scope l0 times I At { block: BlockId(0), op: 0 }..End
scope l1 action Lkotlin/jvm/functions/Function1; At { block: BlockId(0), op: 0 }..End
"
    );
}

#[test]
fn let_call() {
    let Some(text) = decoded(
        "kotlin/StandardKt__StandardKt",
        "let",
        "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
    ) else {
        return;
    };
    assert_eq!(
        text,
        "\
l0 = slot 0 Reference parameter
l1 = slot 1 Reference parameter
b0():
  [-] v0: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [-] v1: Reference(\"Ljava/lang/String;\") = Constant(String(\"block\"))
  [-] invoke Static kotlin/jvm/internal/Intrinsics.checkNotNullParameter(Ljava/lang/Object;Ljava/lang/String;)V (v0, v1)
  [118] v2: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [118] v3: Reference(\"Ljava/lang/Object;\") = load l0
  [118] v4: Reference(\"Ljava/lang/Object;\") = invoke Interface kotlin/jvm/functions/Function1.invoke(Ljava/lang/Object;)Ljava/lang/Object; interface (v2, v3)
  [118] return v4
scope l0 $this$let Ljava/lang/Object; At { block: BlockId(0), op: 0 }..End
scope l1 block Lkotlin/jvm/functions/Function1; At { block: BlockId(0), op: 0 }..End
"
    );
}

/// `Closeable.use(block)`: a `try`/`catch`/`finally` whose handlers cover exactly the blocks of
/// their protected ranges.
#[test]
fn closeable_use_handlers() {
    let Some(text) = decoded(
        "kotlin/io/CloseableKt",
        "use",
        "(Ljava/io/Closeable;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
    ) else {
        return;
    };
    assert_eq!(
        text,
        "\
l0 = slot 0 Reference parameter
l1 = slot 1 Reference parameter
l2 = slot 2 Reference
l3 = slot 3 Reference
l4 = slot 4 Reference
b0():
  [-] v0: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [-] v1: Reference(\"Ljava/lang/String;\") = Constant(String(\"block\"))
  [-] invoke Static kotlin/jvm/internal/Intrinsics.checkNotNullParameter(Ljava/lang/Object;Ljava/lang/String;)V (v0, v1)
  [26] v2: Null = Null
  [26] store l2 v2
  [26] goto b1()
b1():
  [28] v3: Reference(\"Lkotlin/jvm/functions/Function1;\") = load l1
  [28] v4: Reference(\"Ljava/io/Closeable;\") = load l0
  [28] v5: Reference(\"Ljava/lang/Object;\") = invoke Interface kotlin/jvm/functions/Function1.invoke(Ljava/lang/Object;)Ljava/lang/Object; interface (v3, v4)
  [28] store l3 v5
  [28] goto b2()
b2():
  [28] v6: Int = Constant(Int(1))
  [28] invoke Static kotlin/jvm/internal/InlineMarker.finallyStart(I)V (v6)
  [33] v7: Reference(\"Ljava/io/Closeable;\") = load l0
  [33] v8: Null = load l2
  [33] invoke Static kotlin/io/CloseableKt.closeFinally(Ljava/io/Closeable;Ljava/lang/Throwable;)V (v7, v8)
  [33] v9: Int = Constant(Int(1))
  [33] invoke Static kotlin/jvm/internal/InlineMarker.finallyEnd(I)V (v9)
  [33] v10: Reference(\"Ljava/lang/Object;\") = load l3
  [28] return v10
b3(v11: Reference(\"Ljava/lang/Throwable;\")):
  [29] store l4 v11
  [30] v12: Reference(\"Ljava/lang/Throwable;\") = load l4
  [30] store l2 v12
  [31] v13: Reference(\"Ljava/lang/Throwable;\") = load l4
  [31] throw v13
b4(v14: Reference(\"Ljava/lang/Throwable;\")):
  [33] store l4 v14
  [33] goto b5()
b5():
  [33] v15: Int = Constant(Int(1))
  [33] invoke Static kotlin/jvm/internal/InlineMarker.finallyStart(I)V (v15)
  [33] v16: Reference(\"Ljava/io/Closeable;\") = load l0
  [33] v17: Reference(\"Ljava/lang/Throwable;\") = load l2
  [33] invoke Static kotlin/io/CloseableKt.closeFinally(Ljava/io/Closeable;Ljava/lang/Throwable;)V (v16, v17)
  [33] v18: Int = Constant(Int(1))
  [33] invoke Static kotlin/jvm/internal/InlineMarker.finallyEnd(I)V (v18)
  [33] v19: Reference(\"Ljava/lang/Throwable;\") = load l4
  [33] throw v19
handler [b1] -> b3 catch Some(\"java/lang/Throwable\")
handler [b1] -> b4 catch None
handler [b3] -> b4 catch None
handler [b4] -> b4 catch None
scope l4 e Ljava/lang/Throwable; At { block: BlockId(3), op: 1 }..At { block: BlockId(4), op: 0 }
scope l2 exception Ljava/lang/Throwable; At { block: BlockId(1), op: 0 }..End
scope l0 $this$use Ljava/io/Closeable; At { block: BlockId(0), op: 0 }..End
scope l1 block Lkotlin/jvm/functions/Function1; At { block: BlockId(0), op: 0 }..End
"
    );
}

/// Every method body in the standard library decodes. The stdlib holds every construct an inline
/// body compiles to: switches, `invokedynamic`, handlers, monitors and wide values.
#[test]
fn every_stdlib_method_decodes() {
    let Some(mut archive) = stdlib() else {
        return;
    };
    let mut decoded = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("entry");
        let class_name = entry.name().expect("entry name").into_owned();
        let Some(class) = class_name.strip_suffix(".class") else {
            continue;
        };
        if class.starts_with("META-INF/") {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read class");
        let info = classreader::parse_class(&bytes).expect("parse class");
        for method in &info.methods {
            let Some(body) =
                classreader::read_method_code(&bytes, &method.name, &method.descriptor)
            else {
                continue;
            };
            let node = MethodNode::read(method.access, &method.name, &method.descriptor, &body)
                .expect("read method");
            decode(class, &node).unwrap_or_else(|error| {
                panic!("{class}.{}{}: {error:?}", method.name, method.descriptor)
            });
            decoded += 1;
        }
    }
    assert!(
        decoded > 10_000,
        "only {decoded} stdlib methods were decoded"
    );
}
