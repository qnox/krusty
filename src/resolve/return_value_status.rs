//! The return value status of this compilation's own callables: kotlinc's
//! `FirMustUseReturnValueStatusComponent`.
//!
//! A callable's status says whether a caller must use its result. It is resolved from what the
//! declaration is annotated with, the scopes that enclose it, the checker mode, and the status of
//! the declaration it overrides:
//!
//! 1. an `@IgnorableReturnValue` declaration is `ExplicitlyIgnorable`;
//! 2. a declaration whose first overridden declaration must be used must be used;
//! 3. in `full` mode every other declaration must be used, unless it overrides an ignorable or
//!    unspecified declaration;
//! 4. a declaration annotated `@MustUseReturnValues`, or enclosed by a class, a property, or a file
//!    that is, must be used;
//! 5. anything else takes the status of the declaration it overrides, else is `Unspecified`.
//!
//! With the checker disabled only the last rule applies. Annotations are identified by their
//! resolved classes, so an alias or an import never changes the outcome.

use crate::features::ReturnValueCheckerMode;
use crate::fir::{DeclarationId, ResolvedModuleIndex};
use crate::types::{wk, ReturnValueStatus, TypeName};

/// What a declaration's annotations and enclosing scopes say about its return value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeclaredReturnValue {
    /// Marked `@IgnorableReturnValue` (or a library's equivalent) itself.
    ignorable: bool,
    /// Marked `@MustUseReturnValues` (or a library's equivalent) itself or by an enclosing scope.
    must_use: bool,
}

fn ignorable_like(annotation: TypeName) -> bool {
    annotation == wk::ignorable_return_value()
        || annotation == wk::errorprone_can_ignore_return_value()
}

fn must_use_like(annotation: TypeName) -> bool {
    [
        wk::must_use_return_values(),
        wk::errorprone_check_return_value(),
        wk::jetbrains_check_return_value(),
        wk::spring_check_return_value(),
        wk::jooq_check_return_value(),
        wk::findbugs_check_return_value(),
    ]
    .contains(&annotation)
}

impl DeclaredReturnValue {
    /// The ignorability `declaration` states, read from its resolved annotations and those of
    /// every class, property and file around it.
    pub(crate) fn of(index: &ResolvedModuleIndex, declaration: DeclarationId) -> Self {
        let annotated = |declaration| {
            index
                .declaration_annotations(declaration)
                .iter()
                .copied()
                .any(must_use_like)
        };
        let mut must_use = annotated(declaration);
        let mut enclosing = index.declaration_anchor(declaration);
        let mut file = enclosing.map(|anchor| anchor.source);
        while let Some(anchor) = enclosing {
            file = Some(anchor.source);
            let Some(owner) = anchor.owner else { break };
            must_use = must_use || annotated(owner);
            enclosing = index.declaration_anchor(owner);
        }
        if let Some(file) = file {
            must_use = must_use
                || index
                    .file_applied_annotations(file)
                    .iter()
                    .any(|annotation| must_use_like(annotation.annotation));
        }
        DeclaredReturnValue {
            ignorable: index
                .declaration_annotations(declaration)
                .iter()
                .copied()
                .any(ignorable_like),
            must_use,
        }
    }

    /// The status the rules above give a callable that overrides a declaration whose status is
    /// `overridden`, or none.
    pub(crate) fn resolve(
        self,
        mode: ReturnValueCheckerMode,
        overridden: Option<ReturnValueStatus>,
    ) -> ReturnValueStatus {
        let inherited = overridden.unwrap_or_default();
        if mode == ReturnValueCheckerMode::Disabled {
            return inherited;
        }
        if self.ignorable {
            return ReturnValueStatus::ExplicitlyIgnorable;
        }
        if overridden == Some(ReturnValueStatus::MustUse) {
            return ReturnValueStatus::MustUse;
        }
        let overrides_ignorable_or_unspecified = matches!(
            overridden,
            Some(ReturnValueStatus::ExplicitlyIgnorable | ReturnValueStatus::Unspecified)
        );
        if mode == ReturnValueCheckerMode::Full && !overrides_ignorable_or_unspecified {
            return ReturnValueStatus::MustUse;
        }
        if self.must_use {
            return ReturnValueStatus::MustUse;
        }
        inherited
    }
}
