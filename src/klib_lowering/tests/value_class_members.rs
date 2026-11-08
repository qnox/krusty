//! A value class's members are emitted as members of its materialized class, reading the value
//! they are called on through the class's one field.

use super::*;

/// The provider record of the member `name` of the stdlib class `classifier` with `params`.
fn member(
    libraries: &KlibLibraries,
    classifier: &str,
    name: &str,
    params: &[Ty],
) -> crate::libraries::ExternalCallableRealization {
    let class = libraries
        .classifier(type_name(classifier))
        .expect("a stdlib class");
    let target = class.declared_callables[name]
        .functions()
        .iter()
        .find(|function| function.callable.params == params)
        .and_then(|function| function.callable.external_identity)
        .expect("an exact member declaration");
    libraries
        .external_callable(target)
        .expect("the provider realizes its own identity")
}

#[test]
fn a_value_class_member_is_a_member_of_its_class_reading_the_value_through_its_field() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let selected = member(&libraries, "kotlin/time/Duration", "isNegative", &[]);
    let callable = selected.klib_body_callable().expect("a KLIB member");
    let facts = KlibCalleeFacts::with_declarations([callable], []);
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(callable, libraries.declaration_bodies(), &facts)
        .unwrap_or_else(|decline| panic!("`Duration.isNegative`: {decline}"));
    let ir = unit.ir();
    let declared = &ir.functions[function as usize];
    assert_eq!(
        (
            declared.params.as_slice(),
            declared.ret,
            declared.is_static,
            declared.dispatch_receiver
        ),
        (
            &[][..],
            Ty::Boolean,
            false,
            Some(type_name("kotlin/time/Duration"))
        )
    );
    let [class] = &ir.classes[..] else {
        panic!("the unit materializes the value class alone");
    };
    assert_eq!(
        (class.fq_name, class.is_value, class.fields.len()),
        (type_name("kotlin/time/Duration"), true, 1)
    );
    assert_eq!(
        lowered_body(&unit, function),
        "scope {scope {return@Some(0) Lt(call v0!: kotlin/time/Duration.<get-rawValue>(): Long, \
         ImplicitCoercion<Long>(Int(0): Int): Long): Boolean: Nothing}: Nothing}"
    );
    // The private getter no provider records is emitted as its class's other members are.
    let [getter] = (0..ir.functions.len() as FunId)
        .filter(|&id| id != function)
        .collect::<Vec<_>>()[..]
    else {
        panic!("the unit emits the member and the getter it calls alone");
    };
    // Only the member is a function callers outside the unit can name.
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        [function]
    );
    let declared = &ir.functions[getter as usize];
    assert_eq!(
        (
            declared.params.as_slice(),
            declared.ret,
            declared.is_static,
            declared.dispatch_receiver
        ),
        (
            &[][..],
            Ty::Long,
            false,
            Some(type_name("kotlin/time/Duration"))
        )
    );
    assert_eq!(
        lowered_body(&unit, getter),
        "scope {scope {return@Some(0) v0!: kotlin/time/Duration.rawValue: Long: Nothing}: Nothing}"
    );
}

/// One serialized fact of `Duration`'s raw class header a malformed-header control rewrites.
#[derive(Clone, Copy, Debug)]
enum Change {
    None,
    /// `isNegative`'s receiver names `Duration`'s path with another signature mask.
    ReceiverMask,
    /// The private `rawValue` getter's receiver does.
    PrivateReceiverMask,
    /// The `rawValue` property is declared in `Duration.Companion` instead.
    PrivateClass,
    /// The `rawValue` property symbol names another declaration than its getter's property.
    PrivatePropertyGroup,
    /// The `rawValue` getter sits in its property's setter slot.
    PrivateSetterSlot,
}

/// `Duration`'s own declaration tree, decoded afresh from the stdlib and rewritten by `change`,
/// as the only tree of the stdlib's module.
fn duration_bodies(
    root: &std::path::Path,
    contract: &crate::libraries::KlibBodyClassContract,
    change: Change,
) -> KlibDeclarationBodies {
    let archive =
        crate::klib::KlibArchive::open(&root.join("klib/common/stdlib")).expect("the stdlib opens");
    let declaration = KlibIrSignature::Public(contract.signature.clone());
    let mut trees = crate::metadata::klib_ir::read_declaration_trees_where(&archive, |signature| {
        *signature == declaration
    })
    .expect("the stdlib IR decodes")
    .into_trees();
    let [tree] = &mut trees[..] else {
        panic!("`Duration` is one declaration tree");
    };
    let KlibIrMember::Class(class) = tree.declaration else {
        panic!("`Duration`'s tree declares the class");
    };
    let arena = &mut tree.arena;
    let members = arena.class(class).members.clone();
    let is_negative = members
        .iter()
        .find_map(|member| match member {
            KlibIrMember::Function(id) if arena.function(*id).name == "isNegative" => Some(*id),
            _ => None,
        })
        .expect("`Duration` declares `isNegative`");
    let raw_value = members
        .iter()
        .position(|member| matches!(member, KlibIrMember::Property(property) if property.name == "rawValue"))
        .expect("`Duration` declares `rawValue`");
    let KlibIrMember::Property(property) = &members[raw_value] else {
        unreachable!("the member is the property");
    };
    let getter = property.getter.expect("`rawValue` has a getter");
    let remasked_receiver = |arena: &mut KlibIrArena, function| {
        let receiver: &KlibIrFunction = arena.function(function);
        let mut ty = arena
            .ty(receiver.dispatch_receiver.as_ref().expect("a member").ty)
            .clone();
        let KlibIrType::Simple { classifier, .. } = &mut ty else {
            panic!("a class receiver");
        };
        classifier.signature = KlibIrSignature::Public(
            super::constructor_contracts::different_mask(&contract.signature),
        );
        let ty = arena.push_type(ty);
        arena
            .function_mut(function)
            .dispatch_receiver
            .as_mut()
            .unwrap()
            .ty = ty;
    };
    let mut rewritten = members.clone();
    match change {
        Change::None => {}
        Change::ReceiverMask => remasked_receiver(arena, is_negative),
        Change::PrivateReceiverMask => remasked_receiver(arena, getter),
        Change::PrivateClass => {
            // The backing field stays with `Duration`; only the getter's property group moves
            // into the companion, so the value class itself remains well formed.
            let KlibIrMember::Property(stored) = &mut rewritten[raw_value] else {
                unreachable!("the member is the property");
            };
            let mut moved = stored.clone();
            stored.getter = None;
            moved.backing_field = None;
            let moved = KlibIrMember::Property(moved);
            let companion = rewritten
                .iter()
                .find_map(|member| match member {
                    KlibIrMember::Class(id) if arena.class(*id).name == "Companion" => Some(*id),
                    _ => None,
                })
                .expect("`Duration` declares its companion");
            arena.class_mut(companion).members.push(moved);
        }
        Change::PrivatePropertyGroup => {
            let KlibIrMember::Property(property) = &mut rewritten[raw_value] else {
                unreachable!("the member is the property");
            };
            property.base.symbol.signature = declaration.clone();
        }
        Change::PrivateSetterSlot => {
            let KlibIrMember::Property(property) = &mut rewritten[raw_value] else {
                unreachable!("the member is the property");
            };
            property.setter = property.getter.take();
        }
    }
    arena.class_mut(class).members = rewritten;
    KlibDeclarationBodies::from_libraries(vec![(
        contract.module.clone(),
        KlibIrModuleTrees::from_trees(trees).expect("the rewritten tree indexes"),
    )])
    .expect("the rewritten module joins")
}

/// The member and the private getter it calls are proved against `Duration`'s raw class header,
/// mask included, before any semantic name is compared; each malformed header declines and
/// leaves the unit empty.
#[test]
fn value_class_members_authenticate_their_raw_class_header() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let selected = member(&libraries, "kotlin/time/Duration", "isNegative", &[]);
    let callable = selected.klib_body_callable().expect("a KLIB member");
    let contract = callable.class_contract().expect("a class member");
    let facts = KlibCalleeFacts::with_declarations([callable], []);
    let mut unit = DependencyBodyUnit::default();
    let control = duration_bodies(&root, contract, Change::None);
    let function = unit
        .lower_function(callable, &control, &facts)
        .unwrap_or_else(|decline| panic!("the rebuilt `Duration` header lowers: {decline}"));
    assert_eq!(
        lowered_body(&unit, function),
        "scope {scope {return@Some(0) Lt(call v0!: kotlin/time/Duration.<get-rawValue>(): Long, \
         ImplicitCoercion<Long>(Int(0): Int): Long): Boolean: Nothing}: Nothing}"
    );
    let declines = [
        Change::ReceiverMask,
        Change::PrivateReceiverMask,
        Change::PrivateClass,
        Change::PrivatePropertyGroup,
        Change::PrivateSetterSlot,
    ]
    .map(|change| {
        let bodies = duration_bodies(&root, contract, change);
        let mut unit = DependencyBodyUnit::default();
        let decline = unit
            .lower_function(callable, &bodies, &facts)
            .expect_err("a malformed header declines");
        super::demo_package::assert_unit_is_empty(&unit);
        assert!(unit.ir().classes.is_empty(), "{change:?} keeps no class");
        format!("{change:?}: {decline}")
    });
    let receiver = "the serialized member receiver names a different class identity)";
    let member = "the serialized callable is not a unique direct class member)";
    let prefix = "the KLIB body of `kotlin.time.Duration.isNegative` (its serialized declaration disagrees with the selected one: ";
    assert_eq!(
        declines,
        [
            format!("ReceiverMask: {prefix}{receiver}"),
            format!("PrivateReceiverMask: {prefix}{receiver}"),
            format!("PrivateClass: {prefix}{member}"),
            format!("PrivatePropertyGroup: {prefix}{member}"),
            format!("PrivateSetterSlot: {prefix}{member}"),
        ]
    );
}
