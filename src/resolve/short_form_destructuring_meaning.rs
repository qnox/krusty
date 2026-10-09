//! Warnings for positional entries of the parenthesized short destructuring form.
//!
//! Kotlin plans to read `val (a, b) = e`, `for ((a, b) in xs)` and `{ (a, b) -> }` by property
//! name. Under `+DeprecateNameMismatchInShortDestructuringWithParentheses` (and while
//! `EnableNameBasedDestructuringShortForm` is off) kotlinc's `FirDestructuringDeclarationChecker`
//! warns for every positional entry whose meaning that change would break: an underscore, a
//! destructured classifier that is not a data class, a data-class component without a primary
//! constructor property, and an entry whose name differs from the property it reads.

use crate::ast::DestructureEntry;
use crate::types::{CollectionKind, MappedCollection, Ty};

use super::Checker;

const SEE_MORE: &str = "See https://kotl.in/name-based-destructuring for more information.";

/// Why a positional short-form entry would change meaning under name-based destructuring.
enum ShortFormProblem {
    /// The destructured classifier is not a data class and has no well-known property for the
    /// first component.
    NonDataClass,
    /// A data class whose component has no primary-constructor property.
    DataClassCustomComponent,
    /// The entry reads a property with a different name.
    NameMismatch(String),
}

impl Checker<'_> {
    /// Check one positional entry of a parenthesized short-form destructuring. `destructured` is the
    /// type the statement destructures and `component` its 1-based component index.
    pub(super) fn check_parenthesized_short_form_entry(
        &mut self,
        destructured: Ty,
        component: usize,
        entry: &DestructureEntry,
    ) {
        if entry.ignored {
            self.diags.warning(
                entry.name_span,
                format!(
                    "this syntax will be used for name-based destructuring in a future release, \
                     and an underscore without renaming will become an error.\nUse name-based \
                     destructuring syntax '(val x, ...)' and drop the unused entry, or use the new \
                     positional destructuring syntax '[_, ...]'.\n{SEE_MORE}"
                ),
            );
            return;
        }
        let name = &entry.name;
        let message = match self.short_form_problem(destructured, component, name) {
            None => return,
            Some(ShortFormProblem::NameMismatch(property)) => format!(
                "variable name '{name}' differs from accessed property name '{property}'. This \
                 syntax will be used for name-based destructuring in a future release, and this \
                 code will change its meaning.\nUse the full name-based destructuring syntax \
                 '(val {name} = {property}, ...)', the new positional destructuring syntax \
                 '[{name}, ...]', or align the names to prepare for the transition.\n{SEE_MORE}"
            ),
            Some(problem) => {
                let declaration = match problem {
                    ShortFormProblem::NonDataClass => "non-data class",
                    _ => "custom component operators of data class",
                };
                let rendered = self.diagnostic_type_name(destructured, &[destructured]);
                format!(
                    "this syntax will be used for name-based destructuring in a future release \
                     which will stop compiling or change its meaning for {declaration} \
                     '{rendered}'.\nUse the new positional destructuring syntax '[{name}, ...]' to \
                     prepare for the transition.\n{SEE_MORE}"
                )
            }
        };
        self.diags.warning(entry.name_span, message);
    }

    /// kotlinc's `getProblem`: the property a name-based reading would access for `component`, from
    /// the destructured classifier's declaration. A type that is not a classifier type (a type
    /// parameter, an intersection, a function type) has none and is never reported.
    fn short_form_problem(
        &self,
        destructured: Ty,
        component: usize,
        name: &str,
    ) -> Option<ShortFormProblem> {
        let Ty::Obj(classifier, _) = destructured.non_null() else {
            return None;
        };
        let class = self.resolver().classifier(classifier)?;
        let read_only_map_entry = MappedCollection {
            kind: CollectionKind::MapEntry,
            mutable: false,
        };
        let property = if class.is_data {
            class
                .constructors
                .iter()
                .find(|constructor| constructor.is_primary_constructor())
                .and_then(|constructor| constructor.call_sig.param_names.get(component - 1))
                .map(String::as_str)
        } else if class.mapped_collection == Some(read_only_map_entry) {
            // kotlinc names `Map.Entry`'s properties itself (`StandardNames.MAP_ENTRY_KEY`/`_VALUE`).
            match component {
                1 => Some("key"),
                2 => Some("value"),
                _ => None,
            }
        } else {
            None
        };
        match property {
            // Repeating the non-data report on every entry would say nothing new.
            None if !class.is_data => (component == 1).then_some(ShortFormProblem::NonDataClass),
            None => Some(ShortFormProblem::DataClassCustomComponent),
            Some(property) => {
                (property != name).then(|| ShortFormProblem::NameMismatch(property.to_string()))
            }
        }
    }
}
