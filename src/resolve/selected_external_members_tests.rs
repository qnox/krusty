use super::*;
use crate::ast::ExprId;

fn member(raw: u32) -> crate::libraries::LibraryMember {
    let mut member = crate::libraries::enum_values(crate::types::type_name("lib/Id$Companion"));
    member.external_identity = Some(ExternalCallableId::from_raw(raw));
    member
}

#[test]
fn committing_a_selection_records_every_declaration_it_names() {
    let ledger = SelectedExternalMembers::default();
    let mut calls = SelectionTable::<ExprId, ResolvedCall>::new(&ledger);
    let mut lowerings = SelectionTable::<ExprId, ExprLowering>::new(&ledger);

    calls.insert(ExprId(0), ResolvedCall::Companion(member(1)));
    lowerings.insert(
        ExprId(1),
        ExprLowering::Invoke {
            receiver: ExprId(2),
            params: Vec::new(),
            kind: InvokeKind::Operator {
                receiver_ty: crate::types::Ty::Int,
                target: Box::new(ResolvedCall::Companion(member(2))),
                param_default_values: Vec::new(),
            },
        },
    );
    lowerings.insert(
        ExprId(3),
        ExprLowering::AssociatedPropertyRead {
            stable_declaration: None,
            external_identity: Some(ExternalPropertyId::from_raw(3)),
            singleton_dispatch: None,
            owner: crate::types::type_name("lib/Id"),
            name: "v".to_owned(),
            metadata_constant_read: false,
        },
    );

    assert_eq!(
        ledger.snapshot(),
        vec![
            SelectedExternal::Callable(ExternalCallableId::from_raw(1)),
            SelectedExternal::Callable(ExternalCallableId::from_raw(2)),
            SelectedExternal::Property(ExternalPropertyId::from_raw(3)),
        ]
    );
}
