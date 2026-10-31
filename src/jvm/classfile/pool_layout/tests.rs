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
/// A `checkcast` the rewrite removes leaves its class to the `InnerClasses` row, which is interned
/// before the `Code` attribute name.
fn a_removed_holder_class_moves_to_its_inner_classes_row() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    writer.add_inner_class(crate::jvm::classfile::InnerClassSpec {
        inner: "pkg/Ref$LongRef".to_string(),
        outer: Some("pkg/Ref".to_string()),
        name: Some("LongRef".to_string()),
        access: ACC_PUBLIC | ACC_STATIC,
    });
    let desc = "(Lpkg/Ref$LongRef;)Ljava/lang/Object;";
    writer.reserve_method_pool("f", desc, None, &[]);
    add_static(&mut writer, "f", desc, |code, writer| {
        code.aload(0);
        code.checkcast(writer.class_ref("pkg/Ref$LongRef"));
        code.areturn();
    });
    let entries = pool(&writer.finish());
    let position = |entry: &str| {
        entries
            .iter()
            .position(|candidate| candidate == entry)
            .unwrap_or_else(|| panic!("{entry} in {entries:?}"))
    };
    // The row is interned before the `Code` attribute name, as kotlinc writes `InnerClasses`
    // before that name. The removed `checkcast` does not leave the class beside the descriptor.
    assert!(position("(Lpkg/Ref$LongRef;)Ljava/lang/Object;") < position("Class pkg/Ref$LongRef"));
    assert!(position("Class pkg/Ref$LongRef") < position("Class pkg/Ref"));
    assert!(position("Class pkg/Ref") < position("LongRef"));
    assert!(position("LongRef") < position("Code"));
}

#[test]
fn a_constant_only_later_code_still_names_moves_to_that_code() {
    // `fun f(s: String): Any = s as String` interned `java/lang/String` for a cast its rewrite
    // removes; `g` names the class between its casts to `B` and `C`, which is where ASM interns it.
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    let f = "(Ljava/lang/String;)Ljava/lang/Object;";
    writer.reserve_method_pool("f", f, None, &[]);
    add_static(&mut writer, "f", f, |code, writer| {
        code.aload(0);
        code.checkcast(writer.class_ref("java/lang/String"));
        code.areturn();
    });
    let g = "(Ljava/lang/Object;)Ljava/lang/Object;";
    writer.reserve_method_pool("g", g, None, &[]);
    add_static(&mut writer, "g", g, |code, writer| {
        code.aload(0);
        code.checkcast(writer.class_ref("B"));
        code.checkcast(writer.class_ref("java/lang/String"));
        code.checkcast(writer.class_ref("C"));
        code.areturn();
    });
    assert_eq!(
        pool(&writer.finish()),
        [
            "T",
            "Class T",
            "java/lang/Object",
            "Class java/lang/Object",
            "f",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            "g",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            "B",
            "Class B",
            "java/lang/String",
            "Class java/lang/String",
            "C",
            "Class C",
            "Code",
        ]
    );
}

#[test]
fn a_constant_the_last_later_code_names_moves_to_the_end_of_that_code() {
    // As above, but `g` names `java/lang/String` last, and no entry is interned after it: the
    // constant moves past every entry `g` names first, to the end of the pool. The `Code` name,
    // which the writer otherwise interns last, is interned first so that no entry follows.
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    writer.cp.utf8("Code");
    let f = "(Ljava/lang/String;)Ljava/lang/Object;";
    writer.reserve_method_pool("f", f, None, &[]);
    add_static(&mut writer, "f", f, |code, writer| {
        code.aload(0);
        code.checkcast(writer.class_ref("java/lang/String"));
        code.areturn();
    });
    let g = "(Ljava/lang/Object;)Ljava/lang/Object;";
    writer.reserve_method_pool("g", g, None, &[]);
    add_static(&mut writer, "g", g, |code, writer| {
        code.aload(0);
        code.checkcast(writer.class_ref("B"));
        code.checkcast(writer.class_ref("java/lang/String"));
        code.areturn();
    });
    assert_eq!(
        pool(&writer.finish()),
        [
            "T",
            "Class T",
            "java/lang/Object",
            "Class java/lang/Object",
            "Code",
            "f",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            "g",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            "B",
            "Class B",
            "java/lang/String",
            "Class java/lang/String",
        ]
    );
}

#[test]
fn an_orphaned_descriptor_keeps_name_and_type_component_order() {
    // A removed instruction originally interned `()V`. The later EnclosingMethod is its first
    // remaining user, and ASM visits that NameAndType as name, descriptor, composite. Relayout
    // must not put the orphaned descriptor ahead of the fresh method name merely because its old
    // pool index is lower.
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    let before = writer.cp.slot_count();
    writer.reserve_descriptor("()V");
    add_static(&mut writer, "f", "()I", |code, writer| {
        code.push_int(1, writer);
        code.ireturn();
    });
    let after = writer.cp.slot_count();
    writer.set_enclosing_method("Owner", "outer", "()V");
    let class = writer.finish();
    let relaid = [RelaidMethod {
        index: 0,
        added: before + 1..after + 1,
        leading: Vec::new(),
        interned: after + 1..after + 1,
    }];
    let laid_out = relaid_class(&class, &relaid, Unnamed::Dropped, &[], &[])
        .expect("the class reads")
        .expect("the orphan moves to EnclosingMethod");
    let entries = pool(&laid_out);
    let position = |entry: &str| {
        entries
            .iter()
            .position(|candidate| candidate == entry)
            .unwrap_or_else(|| panic!("{entry} in {entries:?}"))
    };
    assert!(position("outer") < position("()V"));
    assert!(position("()V") < position("tag 12"));
}

#[test]
fn a_removed_body_class_moves_to_its_late_inner_class_row() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    writer.reserve_method_name("f");
    writer.reserve_descriptor("()V");
    writer.seed_class("sample/Holder$Removed");
    add_static(&mut writer, "f", "()V", |code, writer| {
        code.push_string("body", writer);
        code.pop();
        code.ret_void();
    });
    writer.add_inner_class(crate::jvm::classfile::InnerClassSpec {
        inner: "sample/Holder$Removed".to_string(),
        outer: Some("sample/Holder".to_string()),
        name: Some("Removed".to_string()),
        access: 0x0019,
    });
    let class = writer.finish();
    // The push/pop rewrite removes `body` entirely. The seeded class survives only because the
    // later InnerClasses row names it, so its first pool position is that row's visit window.
    assert_eq!(
        pool(&class),
        [
            "T",
            "Class T",
            "java/lang/Object",
            "Class java/lang/Object",
            "f",
            "()V",
            "sample/Holder$Removed",
            "Class sample/Holder$Removed",
            "sample/Holder",
            "Class sample/Holder",
            "Removed",
            "Code",
            "InnerClasses",
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
        leading: Vec::new(),
        interned: after_g + 1..after_g + 1,
    }];
    let laid_out = relaid_class(&class, &relaid, Unnamed::Dropped, &[], &[])
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
fn transformation_class_entries_lead_the_rewritten_body() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    let before = writer.cp.slot_count();
    add_static(&mut writer, "f", "()Ljava/lang/String;", |code, writer| {
        code.push_string("body", writer);
        code.areturn();
    });
    let after = writer.cp.slot_count();

    // The coroutine transformer discovers this field after the original body was emitted, but
    // kotlinc visits it immediately before writing the transformed body. Record both the new field
    // name and the already-present descriptor to exercise both sides of that ordering rule.
    writer.cp.start_noting();
    writer.add_field(0, "L$0", "Ljava/lang/String;");
    let leading = writer.cp.take_noted();

    let class = writer.finish();
    let relaid = [RelaidMethod {
        index: 0,
        added: before + 1..after + 1,
        leading,
        interned: after + 1..after + 1,
    }];
    let laid_out = relaid_class(&class, &relaid, Unnamed::Dropped, &[], &[])
        .expect("the class reads")
        .expect("the transformer's field moves ahead of the body");
    let entries = pool(&laid_out);
    let position = |entry: &str| {
        entries
            .iter()
            .position(|candidate| candidate == entry)
            .expect("named pool entry")
    };
    assert!(position("L$0") < position("body"));
    assert!(position("Ljava/lang/String;") < position("body"));
}

/// A class whose `ldc` loads branch around each other, with a line and a local-variable table and
/// stack map frames whose offsets the loads' widths move.
fn branching_loads() -> Vec<u8> {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    let mut code = CodeBuilder::new(1);
    let (second, joined) = (code.new_label(), code.new_label());
    code.mark_line(3);
    code.iload(0);
    code.ifne(second);
    code.mark_line(4);
    code.push_string("a", &mut writer);
    code.areturn();
    code.bind(second);
    code.mark_line(5);
    code.iload(0);
    code.push_int(1, &mut writer);
    code.if_icmpeq(joined);
    code.push_string("b", &mut writer);
    code.areturn();
    code.bind(joined);
    code.mark_line(6);
    code.push_string("c", &mut writer);
    code.areturn();
    code.link();
    writer.add_method(ACC_PUBLIC | ACC_STATIC, "f", "(I)Ljava/lang/Object;", &code);
    writer.set_method_debug(
        "f",
        "(I)Ljava/lang/Object;",
        None,
        &[("x".to_string(), "I".to_string(), 0)],
    );
    writer.finish()
}

/// The loaded entries of `class`'s `ldc` and `ldc_w` instructions, with whether each is narrow.
fn loads(class: &[u8]) -> Vec<(String, bool)> {
    let read = index_slots::read(class).expect("the class reads");
    read.slots
        .iter()
        .filter(|slot| matches!(slot.holder, Holder::Code(_, Part::Operand)))
        .filter(|slot| slot.narrow || class[slot.at - 1] == LDC_W)
        .map(|slot| (spelled(class, &read, slot.index), slot.narrow))
        .collect()
}

#[test]
fn an_ldc_whose_entry_moves_past_one_byte_widens_and_narrows_back() {
    let class = with_integers(&branching_loads(), 300);
    let read = index_slots::read(&class).expect("the class reads");
    assert_eq!(
        loads(&class),
        [
            ("String a".to_string(), true),
            ("String b".to_string(), true),
            ("String c".to_string(), true),
        ]
    );
    // Every string moves past the integers.
    let strings: Vec<u16> = (1..read.entries.len() as u16)
        .filter(|&index| spelled(&class, &read, index).starts_with("String "))
        .collect();
    let mut order: Vec<u16> = (1..read.entries.len() as u16)
        .filter(|index| !strings.contains(index))
        .collect();
    order.extend(&strings);
    let widened = rewrite(&class, &read, &order, &[]).expect("the loads widen");
    assert_eq!(
        loads(&widened),
        [
            ("String a".to_string(), false),
            ("String b".to_string(), false),
            ("String c".to_string(), false),
        ]
    );
    assert_eq!(widened.len(), class.len() + 3);
    // Laying the pool out as it was narrows every load again and carries each jump, switch,
    // line, local and frame back to where it was.
    let read_widened = index_slots::read(&widened).expect("the widened class reads");
    let mut renumbered = vec![0u16; read.entries.len()];
    for (at, &index) in order.iter().enumerate() {
        renumbered[usize::from(index)] = at as u16 + 1;
    }
    let restored: Vec<u16> = (1..read.entries.len())
        .map(|index| renumbered[index])
        .collect();
    assert_eq!(
        rewrite(&widened, &read_widened, &restored, &[]).expect("the loads narrow"),
        class
    );
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
    assert_eq!(
        relayout(class.clone(), &[], Unnamed::Dropped, &[], &[]),
        class
    );
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
    assert_eq!(relayout(padded, &[], Unnamed::Dropped, &[], &[]), class);
}

/// A copied class keeps what its visits interned, named or not.
#[test]
fn a_copied_class_keeps_an_entry_nothing_names() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    add_static(&mut writer, "f", "()Ljava/lang/Object;", |code, _| {
        code.aconst_null();
        code.areturn();
    });
    let class = writer.finish();
    let padded = with_integers(&class, 2);
    assert_eq!(
        relayout(padded.clone(), &[], Unnamed::Kept, &[], &[]),
        padded
    );
}

#[test]
fn a_replaced_copied_enclosure_keeps_only_its_visited_entries() {
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    writer.retain_replaced_enclosing_method("declaration/Owner", "outer", "()V");
    writer.seed_utf8("unrelated seed");
    add_static(&mut writer, "f", "()I", |code, writer| {
        code.push_int(1, writer);
        code.ireturn();
    });
    assert_eq!(
        pool(&writer.finish()),
        [
            "T",
            "Class T",
            "java/lang/Object",
            "Class java/lang/Object",
            "declaration/Owner",
            "Class declaration/Owner",
            "outer",
            "()V",
            "tag 12",
            "f",
            "()I",
            "Code",
        ]
    );
}

#[test]
fn a_rewritten_call_site_follows_its_bootstrap_arguments_and_method() {
    // ASM's `addBootstrapMethod` interns a call site's bootstrap arguments, then its bootstrap
    // method handle, before the `InvokeDynamic` and its name and type. `BootstrapMethods` names
    // them only by attribute index, yet a rewritten method's call site is still placed after them.
    let mut writer = ClassWriter::new("T", "java/lang/Object");
    let desc = "()Ljava/lang/Object;";
    let before_g = writer.cp.slot_count();
    add_static(&mut writer, "g", desc, |code, writer| {
        let argument = writer.method_type("()V");
        let bootstrap = writer.method_handle_static(
            "B",
            "bootstrap",
            "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;",
        );
        let entry = writer.add_bootstrap(bootstrap, vec![argument]);
        let site = writer.invoke_dynamic(entry, "run", "()Ljava/lang/Runnable;");
        code.invokedynamic(site, 0, 1);
        code.areturn();
    });
    let after_g = writer.cp.slot_count();
    let class = writer.finish();
    let relaid = [RelaidMethod {
        index: 0,
        added: before_g + 1..after_g + 1,
        leading: Vec::new(),
        interned: after_g + 1..after_g + 1,
    }];
    let laid_out =
        match relaid_class(&class, &relaid, Unnamed::Dropped, &[], &[]).expect("the class reads") {
            Some(laid_out) => laid_out,
            None => class,
        };
    let entries = pool(&laid_out);
    let position = |tag: &str| {
        entries
            .iter()
            .position(|entry| entry == tag)
            .unwrap_or_else(|| panic!("{tag} in {entries:?}"))
    };
    let (argument, handle, site) = (position("tag 16"), position("tag 15"), position("tag 18"));
    assert!(argument < handle && handle < site, "{entries:?}");
}

/// Tied rows take their places in their classes' pool order; a row outside the group stays put.
#[test]
fn tied_inner_class_rows_follow_their_classes_pool_order() {
    let row = |inner: u16, access: u16| {
        let mut row = inner.to_be_bytes().to_vec();
        row.extend([0, 0, 0, 0]);
        row.extend(access.to_be_bytes());
        row
    };
    let table = |rows: &[(u16, u16)]| {
        let mut table = (rows.len() as u16).to_be_bytes().to_vec();
        for &(inner, access) in rows {
            table.extend(row(inner, access));
        }
        table
    };
    let mut laid_out = table(&[(9, 1), (4, 2), (7, 3), (5, 4)]);
    order_tied_rows(&mut laid_out, &[9, 7, 5]);
    assert_eq!(laid_out, table(&[(5, 4), (4, 2), (7, 3), (9, 1)]));
}
