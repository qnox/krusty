//! Typed context-parameter shape carried by one checked FIR body.

use super::FirBody;

impl FirBody {
    pub fn set_context_value_count(&mut self, count: u32) {
        assert!(self.context_parameter_kinds.is_empty());
        assert!(
            count as usize <= self.context_receiver_types.len(),
            "named context values must be a prefix of context receiver types"
        );
        self.context_parameter_kinds = (0..self.context_receiver_types.len())
            .map(|ordinal| {
                if ordinal < count as usize {
                    crate::ast::ContextParameterKind::Named
                } else {
                    crate::ast::ContextParameterKind::LegacyReceiver
                }
            })
            .collect();
    }

    pub fn set_context_parameter_kinds(&mut self, kinds: Vec<crate::ast::ContextParameterKind>) {
        assert!(self.context_parameter_kinds.is_empty());
        assert_eq!(kinds.len(), self.context_receiver_types.len());
        self.context_parameter_kinds = kinds;
    }

    pub fn context_parameter_kinds(&self) -> &[crate::ast::ContextParameterKind] {
        &self.context_parameter_kinds
    }

    pub fn is_context_value_ordinal(&self, ordinal: usize) -> bool {
        self.context_parameter_kinds.get(ordinal) == Some(&crate::ast::ContextParameterKind::Named)
    }

    pub fn context_value_count(&self) -> u32 {
        u32::try_from(
            self.context_parameter_kinds
                .iter()
                .filter(|kind| **kind == crate::ast::ContextParameterKind::Named)
                .count(),
        )
        .expect("too many context value parameters")
    }
}
