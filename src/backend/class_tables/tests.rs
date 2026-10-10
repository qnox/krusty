use super::fixtures::{add_method, class, field, function, record_override};
use super::*;
use crate::ir::IrFile;

/// A target carrying every `Int` as a machine integer and everything else as a reference, owning
/// no base of its own.
struct IntsAndReferences;

impl Representation for IntsAndReferences {
    fn same(&self, a: Ty, b: Ty) -> bool {
        self.is_reference(a) == self.is_reference(b)
    }

    fn is_reference(&self, ty: Ty) -> bool {
        ty != Ty::Int
    }

    fn owns_base(&self, _: TypeName) -> bool {
        false
    }
}

fn tables(ir: &IrFile) -> Result<ClassTables, Unsupported> {
    build(&IntsAndReferences, ir)
}

#[test]
fn a_root_table_is_kotlin_any_by_role_then_the_reserved_function_slot() {
    let mut ir = IrFile::default();
    let point = class(&mut ir, "Point", "kotlin/Any", 0);
    let tables = tables(&ir).expect("tables");
    assert_eq!(
        tables.table(point).vtable,
        vec![
            Slot::AnyMember(AnyMember::Equals),
            Slot::AnyMember(AnyMember::HashCode),
            Slot::AnyMember(AnyMember::ToString),
            Slot::Abstract,
        ]
    );
    assert_eq!(
        tables.region,
        InterfaceRegion {
            base: 4,
            members: 0
        }
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
            Slot::Abstract,
            Slot::Bridge {
                declared,
                target_slot: Some(6),
                target: narrowed,
            },
            Slot::Function(kept),
            Slot::Function(narrowed),
        ]
    );
    assert_eq!(tables.slot(b, &SlotKey::Function(narrowed)), Some(6));
    assert_eq!(tables.slot(b, &SlotKey::Function(kept)), Some(5));
}

#[test]
fn a_superclass_no_file_declares_is_declined_unless_the_target_owns_it() {
    struct OwnsEverything;
    impl Representation for OwnsEverything {
        fn same(&self, a: Ty, b: Ty) -> bool {
            a == b
        }
        fn is_reference(&self, _: Ty) -> bool {
            true
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
    let owned = build(&OwnsEverything, &ir).expect("tables");
    assert_eq!(owned.table(error).superclass, None);
}

#[test]
fn an_interface_member_has_one_number_in_every_implementing_table() {
    let mut ir = IrFile::default();
    let named = class(&mut ir, "Named", "kotlin/Any", 0);
    ir.classes[named as usize].is_interface = true;
    let declared = add_method(
        &mut ir,
        named,
        function("name", "Named", vec![], Ty::String, true),
    );
    let mut implementors = Vec::new();
    for (name, extra) in [("First", false), ("Second", true)] {
        let implementor = class(&mut ir, name, "kotlin/Any", 0);
        ir.classes[implementor as usize].interfaces = vec!["Named"].into();
        if extra {
            add_method(
                &mut ir,
                implementor,
                function("own", name, vec![], Ty::Int, false),
            );
        }
        let implementation = add_method(
            &mut ir,
            implementor,
            function("name", name, vec![], Ty::String, false),
        );
        record_override(&mut ir, implementor, implementation, declared);
        let owner = ir.classes[implementor as usize].fq_name_id();
        for edge in ir.function_overrides.get_mut(&owner).into_iter().flatten() {
            edge.overridden_is_interface = true;
        }
        implementors.push((implementor, implementation));
    }
    let tables = tables(&ir).expect("tables");
    // `Second`'s own two methods put every class's interface region above slot 5.
    assert_eq!(
        tables.region,
        InterfaceRegion {
            base: 6,
            members: 1
        }
    );
    assert_eq!(
        tables.slot(named, &SlotKey::Function(declared)),
        Some(6),
        "the interface's own spelling names the shared number"
    );
    for (implementor, implementation) in implementors {
        let table = tables.table(implementor);
        assert_eq!(table.vtable.len(), 7);
        assert_eq!(table.vtable[6], Slot::Function(implementation));
    }
}
