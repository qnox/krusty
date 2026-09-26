use super::*;
use crate::jvm::classfile::{ClassWriter, CodeBuilder, ACC_PUBLIC, ACC_STATIC};

/// The pool of `class`, one line per entry in index order, with each named entry spelled out.
fn pool(class: &[u8]) -> Vec<String> {
    let read = index_slots::read(class).expect("the class reads");
    (1..read.entries.len())
        .filter(|&index| read.entries[index].is_some())
        .map(|index| spelled(class, &read, index as u16))
        .collect()
}

fn spelled(class: &[u8], read: &ClassSlots, index: u16) -> String {
    let entry = read.entries[usize::from(index)].as_ref().expect("an entry");
    let bytes = &class[entry.range.clone()];
    let named: Vec<String> = entry
        .components
        .iter()
        .map(|&(_, component)| spelled(class, read, component))
        .collect();
    match bytes[0] {
        1 => String::from_utf8(bytes[3..].to_vec()).expect("utf8"),
        3 => format!(
            "Integer {}",
            i32::from_be_bytes(bytes[1..5].try_into().unwrap())
        ),
        7 => format!("Class {}", named[0]),
        8 => format!("String {}", named[0]),
        tag => format!("tag {tag}"),
    }
}

fn add_static(
    writer: &mut ClassWriter,
    name: &str,
    desc: &str,
    emit: impl FnOnce(&mut CodeBuilder, &mut ClassWriter),
) {
    let mut code = CodeBuilder::new(1);
    emit(&mut code, writer);
    code.link();
    writer.add_method(ACC_PUBLIC | ACC_STATIC, name, desc, &code);
}

#[test]
fn a_cast_a_rewrite_removes_leaves_no_entry_behind() {
    // `fun f(s: String): Any = s as String`: the cast goes, and with it `java/lang/String`.
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    add_static(
        &mut writer,
        "f",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        |code, writer| {
            code.aload(0);
            code.checkcast(writer.class_ref("java/lang/String"));
            code.areturn();
        },
    );
    assert_eq!(
        pool(&writer.finish()),
        [
            "T",
            "Class T",
            "java/lang/Object",
            "Class java/lang/Object",
            "f",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            "Code",
        ]
    );
}

#[test]
fn a_rewritten_method_s_entries_are_placed_in_asm_order() {
    // `g`'s local was interned ahead of the constant its code loads; ASM interns an instruction's
    // operand before the local-variable table.
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    let desc = "(Ljava/lang/Object;)Ljava/lang/Object;";
    add_static(&mut writer, "f", desc, |code, writer| {
        code.push_string("a", writer);
        code.areturn();
    });
    let before_g = writer.cp.slot_count();
    writer.reserve_method_name("g");
    writer.reserve_method_lvt(&[("x".to_string(), "Ljava/lang/Object;".to_string(), 0)]);
    add_static(&mut writer, "g", desc, |code, writer| {
        code.push_string("b", writer);
        code.areturn();
        code.add_local_entry(0, None, 0, "x", "Ljava/lang/Object;");
    });
    let after_g = writer.cp.slot_count();
    let class = writer.finish();
    let emitted = [
        "T",
        "Class T",
        "java/lang/Object",
        "Class java/lang/Object",
        "a",
        "String a",
        "f",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        "g",
        "x",
        "Ljava/lang/Object;",
        "b",
        "String b",
        "Code",
        "LocalVariableTable",
    ];
    assert_eq!(pool(&class), emitted);
    let relaid = [RelaidMethod {
        index: 1,
        added: before_g + 1..after_g + 1,
        interned: after_g + 1..after_g + 1,
    }];
    let laid_out = relaid_class(&class, &relaid)
        .expect("the class reads")
        .expect("the pool changes");
    let mut expected = emitted.to_vec();
    expected[9..13].copy_from_slice(&["b", "String b", "x", "Ljava/lang/Object;"]);
    assert_eq!(pool(&laid_out), expected);
    // Every index moved with its entry: the class reads back with the same slots.
    let read = index_slots::read(&laid_out).expect("the laid-out class reads");
    let spelled_slots = |class: &[u8], read: &ClassSlots| -> Vec<String> {
        read.slots
            .iter()
            .map(|slot| spelled(class, read, slot.index))
            .collect()
    };
    assert_eq!(
        spelled_slots(&laid_out, &read),
        spelled_slots(&class, &index_slots::read(&class).expect("the class reads"))
    );
}

#[test]
fn an_ldc_whose_entry_would_move_past_one_byte_keeps_the_pool() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    add_static(&mut writer, "f", "()Ljava/lang/Object;", |code, writer| {
        code.push_string("a", writer);
        code.areturn();
    });
    let class = with_integers(&writer.finish(), 300);
    let read = index_slots::read(&class).expect("the class reads");
    let loaded = read
        .slots
        .iter()
        .find(|slot| slot.narrow)
        .expect("the ldc operand")
        .index;
    let mut order: Vec<u16> = (1..read.entries.len() as u16).collect();
    order.retain(|&index| index != loaded);
    order.push(loaded);
    assert_eq!(rewrite(&class, &read, &order), Err(Kept::NarrowOperand));
}

/// `class` with `count` distinct integers appended to its pool.
fn with_integers(class: &[u8], count: u16) -> Vec<u8> {
    let read = index_slots::read(class).expect("the class reads");
    let entries = read.entries.len() as u16;
    let mut out = class[..8].to_vec();
    out.extend_from_slice(&(entries + count).to_be_bytes());
    out.extend_from_slice(&class[10..read.pool_end]);
    for value in 0..i32::from(count) {
        out.push(3);
        out.extend_from_slice(&(value + 1_000_000).to_be_bytes());
    }
    out.extend_from_slice(&class[read.pool_end..]);
    out
}

#[test]
fn a_class_with_an_attribute_the_reader_does_not_know_is_not_read() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    add_static(&mut writer, "f", "()Ljava/lang/Object;", |code, _| {
        code.aconst_null();
        code.areturn();
    });
    let mut class = with_integers(&writer.finish(), 1);
    let at = class
        .windows(4)
        .position(|window| window == b"Code")
        .expect("the attribute name");
    class[at + 1] = b'x';
    assert_eq!(
        index_slots::read(&class).err(),
        Some(Unread::UnknownAttribute("Cxde".to_string()))
    );
    // The unreferenced integer stays: nothing in the class was renumbered.
    assert_eq!(relayout(class.clone(), &[]), class);
}

#[test]
fn an_entry_nothing_names_is_dropped() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    add_static(&mut writer, "f", "()Ljava/lang/Object;", |code, _| {
        code.aconst_null();
        code.areturn();
    });
    let class = writer.finish();
    let padded = with_integers(&class, 2);
    assert_eq!(relayout(padded, &[]), class);
}
