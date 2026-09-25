//! The value classes an [`IrFile`] knows by name: its own declarations and the checked declarations
//! of other files and dependencies. These are semantic facts; how a target represents a value class
//! (for example the JVM's native unsigned carriers) is the target's own question.

use super::IrFile;
use crate::types::{Ty, TypeName};

impl IrFile {
    pub fn insert_external_value_class_name(&mut self, internal: TypeName, underlying: Ty) {
        if let Some(recorded) = self.external_value_classes.get(&internal) {
            assert_eq!(
                *recorded, underlying,
                "checked providers disagreed about one value-class declaration"
            );
            return;
        }
        self.external_value_classes.insert(internal, underlying);
    }

    pub fn external_value_class_name(&self, internal: TypeName) -> Option<&Ty> {
        self.external_value_classes.get(&internal)
    }

    pub fn has_external_value_class_name(&self, internal: TypeName) -> bool {
        self.external_value_class_name(internal).is_some()
    }

    /// Every value class this IR knows: the file's own and the external ones its checked facts name.
    pub(crate) fn value_class_names(&self) -> impl Iterator<Item = TypeName> + '_ {
        self.classes
            .iter()
            .filter(|class| class.is_value)
            .map(|class| class.fq_name)
            .chain(self.external_value_classes.keys().copied())
    }

    /// Return a value class's declared, one-level underlying semantic type without making callers
    /// branch on whether the declaration belongs to this source file or external checked facts.
    pub(crate) fn value_class_underlying_name(&self, internal: TypeName) -> Option<Ty> {
        self.external_value_class_name(internal)
            .copied()
            .or_else(|| {
                self.classes
                    .iter()
                    .find(|class| class.is_value && class.fq_name == internal)
                    .and_then(|class| class.fields.first().map(|field| field.ty))
            })
    }

    /// Follow the exact source/external value-class facts already assembled in this IR to their
    /// terminal semantic underlying type.
    ///
    /// This is the narrow consumer contract for common passes and plugins. It does not select a
    /// target carrier, boxing policy, descriptor, or storage layout; those remain backend-owned.
    pub(crate) fn terminal_value_class_underlying(&self, ty: Ty) -> Option<Ty> {
        crate::value_classes::terminal_underlying(ty, &|classifier| {
            self.value_class_underlying_name(classifier)
        })
    }

    /// Whether `internal` names a value class — an in-IR (same-file) one OR an external/other-module one.
    /// The single name-keyed value-class test for the unified [`IrExpr::New`] (which no longer carries a
    /// same-file `ClassId` to branch on).
    pub fn is_value_class_name(&self, internal: TypeName) -> bool {
        self.classes
            .iter()
            .any(|c| c.is_value && c.fq_name == internal)
            || self.has_external_value_class_name(internal)
    }
}
