use super::fixtures::{
    add_method, class, field, function, record_external_interface_override, record_override,
};
use super::*;
use crate::ir::IrFile;

/// A target carrying every `Int` as a machine integer and everything else as a reference, owning
/// no base of its own.
struct IntsAndReferences;

impl Representation for IntsAndReferences {
    fn same(&self, a: Ty, b: Ty) -> bool {
        (a != Ty::Int) == (b != Ty::Int)
    }

    fn owns_base(&self, _: TypeName) -> bool {
        false
    }
}

fn tables(ir: &IrFile) -> Result<NumberedClassTables, Unsupported> {
    build_numbered(
        &IntsAndReferences,
        ir,
        &super::module_slots::ModuleSlotPlan::empty(),
        &mut Default::default(),
        &|_| None,
    )
}

#[test]
fn a_root_table_is_kotlin_any_by_role() {
    let mut ir = IrFile::default();
    let point = class(&mut ir, "Point", "kotlin/Any", 0);
    let tables = tables(&ir).expect("tables");
    assert_eq!(
        tables.table(point).vtable,
        vec![
            Slot::AnyMember(AnyMember::Equals),
            Slot::AnyMember(AnyMember::HashCode),
            Slot::AnyMember(AnyMember::ToString),
        ]
    );
    assert_eq!(tables.table(point).superclass, None);
    assert_eq!(tables.table(point).first_field, 0);
}

#[test]
fn own_fields_follow_every_superclass_field() {
    let mut ir = IrFile::default();
    let a = class(&mut ir, "A", "kotlin/Any", 0);
    ir.classes[a as usize].fields = vec![field("x", Ty::Int), field("y", Ty::String)];
    let b = class(&mut ir, "B", "A", 1);
    ir.classes[b as usize].fields = vec![field("z", Ty::Long)];
    let c = class(&mut ir, "C", "B", 2);
    let tables = tables(&ir).expect("tables");
    assert_eq!(tables.table(b).superclass, Some(a));
    assert_eq!(tables.table(b).first_field, 2);
    assert_eq!(tables.table(c).first_field, 3);
    assert_eq!(tables.order, vec![a, b, c]);
}

#[test]
fn an_override_carried_differently_takes_its_own_slot_and_bridges_the_base_slot() {
    let mut ir = IrFile::default();
    let a = class(&mut ir, "A", "kotlin/Any", 0);
    let b = class(&mut ir, "B", "A", 1);
    let declared = add_method(
        &mut ir,
        a,
        function("get", "A", vec![], Ty::obj("kotlin/Any"), false),
    );
    let same = add_method(
        &mut ir,
        a,
        function("put", "A", vec![Ty::obj("kotlin/Any")], Ty::Unit, false),
    );
    let narrowed = add_method(&mut ir, b, function("get", "B", vec![], Ty::Int, false));
    let kept = add_method(
        &mut ir,
        b,
        function("put", "B", vec![Ty::String], Ty::Unit, false),
    );
    record_override(&mut ir, b, narrowed, declared);
    record_override(&mut ir, b, kept, same);
    let tables = tables(&ir).expect("tables");
    let table = tables.table(b);
    assert_eq!(
        table.vtable,
        vec![
            Slot::AnyMember(AnyMember::Equals),
            Slot::AnyMember(AnyMember::HashCode),
            Slot::AnyMember(AnyMember::ToString),
            Slot::Bridge {
                declared,
                target_slot: Some(5),
                target: narrowed,
            },
            Slot::Function(kept),
            Slot::Function(narrowed),
        ]
    );
    assert_eq!(tables.slot(b, &SlotKey::Function(narrowed)), Some(5));
    assert_eq!(tables.slot(b, &SlotKey::Function(kept)), Some(4));
}

#[test]
fn a_superclass_no_file_declares_is_declined_unless_the_target_owns_it() {
    struct OwnsEverything;
    impl Representation for OwnsEverything {
        fn same(&self, a: Ty, b: Ty) -> bool {
            a == b
        }
        fn owns_base(&self, _: TypeName) -> bool {
            true
        }
    }
    let mut ir = IrFile::default();
    let error = class(&mut ir, "Failure", "kotlin/Throwable", 0);
    assert_eq!(
        tables(&ir).map(|_| ()),
        Err(
            "a superclass declared outside this file (`Failure` extends `kotlin/Throwable`)"
                .to_string()
        )
    );
    let owned = build_numbered(
        &OwnsEverything,
        &ir,
        &super::module_slots::ModuleSlotPlan::empty(),
        &mut Default::default(),
        &|_| None,
    )
    .expect("tables");
    assert_eq!(owned.table(error).superclass, None);
}

/// An interface of `ir` with abstract members `names`, which no module numbering knows: what a
/// KLIB lowering unit installs.
fn installed_interface(ir: &mut IrFile, name: &str, names: &[&str]) -> (ClassId, Vec<FunId>) {
    let id = class(ir, name, "kotlin/Any", 0);
    ir.classes[id as usize].is_interface = true;
    let members = names
        .iter()
        .map(|member| add_method(ir, id, function(member, name, vec![Ty::Int], Ty::Int, true)))
        .collect();
    (id, members)
}

#[test]
fn an_installed_interface_numbers_its_members_in_declaration_order() {
    let mut ir = IrFile::default();
    let (first, first_members) = installed_interface(&mut ir, "Sink", &["put", "peek"]);
    let (second, second_members) = installed_interface(&mut ir, "Source", &["take"]);
    let numbered = tables(&ir).expect("an installed interface is numbered by the IR it lives in");
    let numbers = &numbered.numbers;
    let key = |function| super::module_slots::PlanKey::Installed(function);
    assert_eq!(
        [first_members[0], first_members[1], second_members[0]]
            .map(|function| numbers.number(key(function))),
        [Some(0), Some(1), Some(2)]
    );
    // The module's own count: installed numbers are the module's files' to share beyond it.
    assert_eq!(numbers.count(), 0);
    assert_eq!(
        numbers.numbers_of(&[ir.classes[second as usize].fq_name].into_iter().collect()),
        vec![2]
    );
    assert_eq!(
        numbers
            .function_carriers(key(first_members[0]))
            .map(|(parameters, result)| (parameters.to_vec(), result)),
        Some((vec![Ty::Int], Ty::Int))
    );
    let _ = first;
}

/// Two files of one module share the numbers beyond the module's own: an interface both install
/// keeps its numbers in each, an interface only one installs takes numbers no other member has,
/// and a number a target takes afterwards is fresh too.
#[test]
fn installed_interfaces_of_two_files_share_one_numbering() {
    let mut extended = super::module_interfaces::ExtendedNumbers::default();
    let mut numbered = |names: &[(&str, &[&str])]| {
        let mut ir = IrFile::default();
        let members = names
            .iter()
            .map(|(name, members)| installed_interface(&mut ir, name, members).1)
            .collect::<Vec<_>>();
        let tables = build_numbered(
            &IntsAndReferences,
            &ir,
            &super::module_slots::ModuleSlotPlan::empty(),
            &mut extended,
            &|_| None,
        )
        .expect("installed interfaces are numbered");
        members
            .iter()
            .map(|functions| {
                functions
                    .iter()
                    .map(|&function| {
                        tables
                            .numbers
                            .number(super::module_slots::PlanKey::Installed(function))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let first = numbered(&[("Sink", &["put", "peek"])]);
    let second = numbered(&[("Source", &["take"]), ("Sink", &["put", "peek"])]);
    assert_eq!(first, vec![vec![Some(0), Some(1)]]);
    assert_eq!(second, vec![vec![Some(2)], vec![Some(0), Some(1)]]);
    assert_eq!(extended.fresh(0), 3);
}

#[test]
fn an_installed_interface_is_the_only_one_its_itable_implements() {
    let mut ir = IrFile::default();
    let (sink, _) = installed_interface(&mut ir, "Sink", &["put"]);
    let numbered = tables(&ir).expect("tables");
    assert_eq!(
        numbered.interface_tables.implemented[sink as usize],
        vec![ir.classes[sink as usize].fq_name]
    );
}

#[test]
fn an_installed_interface_declared_before_its_installed_base_is_refused() {
    let mut ir = IrFile::default();
    let (derived, _) = installed_interface(&mut ir, "Derived", &["take"]);
    let (_, _) = installed_interface(&mut ir, "Base", &["put"]);
    ir.classes[derived as usize]
        .interfaces
        .push_name(crate::types::type_name("Base"));
    assert_eq!(
        tables(&ir).map(|_| ()),
        Err(
            "an installed interface extending one with module-wide numbers (`Derived`)".to_string()
        )
    );
}

#[test]
fn an_installed_interface_extending_another_installed_one_is_refused() {
    let mut ir = IrFile::default();
    let (_, _) = installed_interface(&mut ir, "Base", &["put"]);
    let (derived, _) = installed_interface(&mut ir, "Derived", &["take"]);
    ir.classes[derived as usize]
        .interfaces
        .push_name(crate::types::type_name("Base"));
    assert_eq!(
        tables(&ir).map(|_| ()),
        Err(
            "an installed interface extending one with module-wide numbers (`Derived`)".to_string()
        )
    );
}

#[test]
fn a_value_classs_own_member_that_overrides_nothing_takes_no_table_slot() {
    // Every support unit that emits some of a value class's members must lay its table out
    // alike, so a member no one overrides is called directly and carries no slot.
    let mut plain = IrFile::default();
    let ordinary = class(&mut plain, "Box", "kotlin/Any", 0);
    add_method(
        &mut plain,
        ordinary,
        function("own", "Box", vec![], Ty::Int, false),
    );
    let mut value = IrFile::default();
    let boxed = class(&mut value, "Box", "kotlin/Any", 0);
    value.classes[boxed as usize].is_value = true;
    add_method(
        &mut value,
        boxed,
        function("own", "Box", vec![], Ty::Int, false),
    );
    let slots = |ir: &IrFile, class| tables(ir).expect("tables").table(class).vtable.len();
    assert_eq!(slots(&plain, ordinary), slots(&value, boxed) + 1);
}

#[test]
fn a_value_classs_member_implementing_a_dependency_interface_keeps_its_slot() {
    // `UIntArray.iterator` implements `Collection.iterator`, which the program names only by
    // an override edge: interface dispatch reaches the member through its slot.
    let mut value = IrFile::default();
    let boxed = class(&mut value, "Box", "kotlin/Any", 0);
    value.classes[boxed as usize].is_value = true;
    let own = add_method(
        &mut value,
        boxed,
        function("own", "Box", vec![], Ty::Int, false),
    );
    let implemented = add_method(
        &mut value,
        boxed,
        function("size", "Box", vec![], Ty::Int, false),
    );
    record_external_interface_override(&mut value, boxed, implemented);
    let tables = tables(&value).expect("tables");
    let vtable = &tables.table(boxed).vtable;
    assert_eq!(vtable.len(), 4);
    assert_eq!(vtable[3], Slot::Function(implemented));
    assert!(!vtable.contains(&Slot::Function(own)));
}
