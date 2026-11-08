use super::*;

#[test]
fn inline_function_and_accessor_carry_nested_local_classifier_bodies() {
    let sources = [
        (
            r#"inline fun first(): String = object {
                       fun read(): String {
                           abstract class Abstract
                           open class Open
                           data class Data(val value: Int)
                           Open()
                           Data(1)
                           return "O"
                       }
                   }.read()

                   inline val second: String get() = object {
                       fun read(): String {
                           fun local() {}
                           class Local
                           local()
                           Local()
                           return "K"
                       }
                   }.read()"#,
            "Library",
        ),
        ("fun use(): String = first() + second", "Consumer"),
    ];
    let library = lower_source_from_set(&sources, 0);
    let consumer = lower_source_from_set(&sources, 1);

    assert!(library.foreign_inline_templates.is_empty());
    assert_eq!(
        library
            .classes
            .iter()
            .filter(|class| class.is_local_class && !class.is_anonymous_object)
            .count(),
        4,
    );
    assert_eq!(
        library
            .classes
            .iter()
            .filter(|class| class.is_anonymous_object)
            .count(),
        2,
    );
    assert_eq!(library.local_class_name_provenance.len(), 6);
    assert!(library
        .local_class_name_provenance
        .values()
        .any(|provenance| provenance.segments.last().map(String::as_str) == Some("Local")));
    assert!(!consumer.foreign_inline_templates.is_empty());
    for class in &consumer.classes {
        assert!(
            class.fields.len() >= class.ctor_args.iter().filter(|argument| argument.is_field).count(),
            "every selected constructor storage argument must have an explicit common-IR field: {class:?}",
        );
    }
}

#[test]
fn original_class_context_distinguishes_members_initializers_and_executable_bodies() {
    use crate::ir::IrClassDeclarationContext as Context;

    let ir = lower_single_source_with_jvm_stdlib(
        r#"class Contexts {
            private val property = object { class Member }
            init { val fromInit = object {} }
            constructor() { val fromConstructor = object {} }
            val getter: Any get() = object {}
            fun factory(): Any {
                class Local
                Local()
                return object {}
            }
        }"#,
        "ClassDeclarationContexts",
    );
    let owner = ir
        .classes
        .iter()
        .find(|class| class.declaration_context == Some(Context::TopLevel))
        .expect("the source class is top level")
        .fq_name;
    let contexts = ir
        .local_class_name_provenance
        .iter()
        .map(|(&class, provenance)| {
            (
                provenance.segments.last().unwrap().as_str(),
                ir.classes[class as usize].declaration_context.unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        contexts,
        std::collections::BTreeMap::from([
            ("property", Context::ClassInitializer { owner }),
            ("fromInit", Context::ClassInitializer { owner }),
            ("fromConstructor", Context::Executable),
            ("getter", Context::Executable),
            ("factory", Context::Executable),
            ("Local", Context::Executable),
            ("Member", Context::Member),
        ])
    );
    let nested_members = ir
        .classes
        .iter()
        .filter(|class| class.is_local_class && class.declaration_context == Some(Context::Member))
        .collect::<Vec<_>>();
    assert_eq!(nested_members.len(), 1);
    assert!(!nested_members[0].is_anonymous_object);
    assert!(nested_members[0].enclosure.is_none());
}

#[test]
fn enum_entry_initialization_retains_the_selected_entry_owner() {
    use crate::ir::IrClassDeclarationContext as Context;

    let ir = lower_single_source(
        include_str!("../../../tests/box_regressions/deepInnerClassInEnumEntryClass2.kt"),
        "DeepEnumEntryContext",
    );
    let entry = ir.classes.iter().find(|class| class.is_enum_entry).unwrap();
    assert_eq!(entry.declaration_context, Some(Context::Member));
    let anonymous = ir
        .classes
        .iter()
        .find(|class| class.is_anonymous_object)
        .unwrap();
    assert_eq!(
        anonymous.declaration_context,
        Some(Context::ClassInitializer {
            owner: entry.fq_name
        })
    );
}

#[test]
fn selected_enum_properties_apply_their_registered_entry_receiver() {
    let ir = lower_single_source_with_jvm_stdlib(
        include_str!("../../../tests/box_regressions/enumEntryReceiverFunctionProperty.kt"),
        "EnumEntryReceiverFunctionProperty",
    );
    let entries = ir
        .checked_enum_entry_classes
        .values()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(entries.len(), 1, "one complete-program entry subclass");
    let mut selected_reads = 0;
    for expression in &ir.exprs {
        let IrExpr::Checked(operation) = expression else {
            continue;
        };
        let (target, receiver) = match &**operation {
            crate::ir::IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                ..
            }
            | crate::ir::IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                ..
            }
            | crate::ir::IrCheckedOperation::BackingFieldRead {
                target,
                dispatch_receiver,
            }
            | crate::ir::IrCheckedOperation::BackingFieldWrite {
                target,
                dispatch_receiver,
                ..
            }
            | crate::ir::IrCheckedOperation::LateinitFieldRead {
                target,
                dispatch_receiver,
            } => (target, dispatch_receiver),
            _ => continue,
        };
        let property = ir
            .checked_properties
            .get(target)
            .expect("selected property declaration");
        let Some(class) = property.class.filter(|class| entries.contains(class)) else {
            continue;
        };
        let receiver = receiver.expect("an entry-owned property needs its selected instance");
        let IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            type_operand,
            ..
        } = ir.expr(receiver)
        else {
            panic!("the selected entry's receiver application must precede property use")
        };
        assert_eq!(
            *type_operand,
            Ty::obj_name(ir.classes[class as usize].fq_name_id())
        );
        selected_reads += 1;
    }
    assert!(
        selected_reads > 0,
        "the whole program must exercise entry-owned operations"
    );
}
