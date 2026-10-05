//! A classifier's interface delegations as its primary constructor evaluates them.
//!
//! The finalized classifier header fixes which interfaces are delegated and which declarations the
//! generated forwarders override. Checking the constructor types each delegate value; the calls a
//! forwarder makes on that value are selected from its type here (see
//! [`super::interface_delegation::delegate_member_calls`]).

use super::*;

impl Checker<'_> {
    /// The delegated interface of each `by` clause of `cl`, from its finalized header.
    pub(super) fn finalized_delegation_interfaces(
        &mut self,
        d: DeclId,
        current_owner: Option<TypeName>,
        cl: &ClassDecl,
    ) -> Vec<Ty> {
        let interfaces = if self.active_declarations.is_some() {
            self.finalized_interface_delegations(d).map(|delegations| {
                delegations
                    .iter()
                    .map(|delegation| delegation.interface.get())
                    .collect::<Vec<_>>()
            })
        } else {
            current_owner
                .and_then(|owner| {
                    self.module
                        .legacy_symbols()
                        .and_then(|symbols| symbols.class_by_type_name(owner))
                })
                .map(|class| class.delegated_interfaces.clone())
        };
        match interfaces {
            Some(interfaces) if interfaces.len() == cl.interface_delegations.len() => interfaces,
            Some(interfaces) => {
                self.diags.error(
                    cl.span,
                    format!(
                        "internal error: finalized interface-delegation count {} does not match source count {}",
                        interfaces.len(),
                        cl.interface_delegations.len(),
                    ),
                );
                Vec::new()
            }
            None => {
                self.diags.error(
                    cl.span,
                    "internal error: interface delegation has no finalized classifier header",
                );
                Vec::new()
            }
        }
    }

    fn finalized_interface_delegations(
        &self,
        d: DeclId,
    ) -> Option<&[crate::fir::ResolvedInterfaceDelegation]> {
        let (active, index) = self.active_declarations.zip(self.resolved_index)?;
        let declaration = active.canonical_classifier_declaration(d, index)?;
        Some(&index.classifier_header(declaration)?.interface_delegations)
    }

    /// Select the calls the forwarders of delegation `ordinal` make on its checked delegate value.
    /// Only a finalized header carries forwarders; a classifier still being declared has none.
    pub(super) fn record_delegate_calls(
        &mut self,
        d: DeclId,
        ordinal: usize,
        value: ExprId,
        delegate: Ty,
    ) {
        let Some(index) = self.resolved_index else {
            return;
        };
        let Some(delegation) = self
            .finalized_interface_delegations(d)
            .and_then(|delegations| delegations.get(ordinal))
        else {
            return;
        };
        let calls = super::interface_delegation::delegate_member_calls(
            &self.fed_source(),
            index,
            delegation,
            delegate,
        );
        match calls {
            Some(calls) => {
                self.interface_delegate_calls.insert(value, calls);
            }
            None => self.diags.error(
                self.span(value),
                "internal error: the delegate does not implement a forwarded interface member",
            ),
        }
    }
}
