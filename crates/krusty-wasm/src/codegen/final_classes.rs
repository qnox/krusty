//! The `kotlin.Any` members each final class of the source module answers itself.
//!
//! A later file reaches an instance of an earlier file's final class only through that class's
//! dispatch table. A final class's instances all dispatch exactly as the class does, so a call
//! of one of those members on such an instance is the class's own implementation.

use krusty::backend::class_tables::{AnyMember, NumberedClassTables, Slot};
use krusty::fx_hash::HashMap;
use krusty::ir::{ClassId, IrFile};
use krusty::types::TypeName;

#[derive(Default)]
pub(super) struct FinalClassAnswers {
    members: HashMap<TypeName, Vec<AnyMember>>,
}

impl FinalClassAnswers {
    /// Record the members of `kotlin.Any` each final class of `ir` answers with its own slot.
    pub(super) fn record_file(&mut self, ir: &IrFile, tables: &NumberedClassTables) {
        for (id, class) in ir.classes.iter().enumerate() {
            if class.is_interface || class.is_open || class.is_abstract {
                continue;
            }
            let vtable = &tables.table(id as ClassId).vtable;
            let answered = AnyMember::ALL
                .into_iter()
                .filter(|member| vtable[member.slot() as usize] != Slot::AnyMember(*member))
                .collect();
            self.members.insert(class.fq_name, answered);
        }
    }

    /// Whether every instance of `classifier`, a final class an earlier file declared, answers
    /// `member` with its class's own implementation.
    pub(super) fn answers(&self, classifier: TypeName, member: AnyMember) -> bool {
        self.members
            .get(&classifier)
            .is_some_and(|members| members.contains(&member))
    }
}
