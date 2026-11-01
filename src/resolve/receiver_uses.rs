//! Semantic uses of exact lexical receiver bindings, counted and kept in the order they happened.
//!
//! Capture discovery snapshots the counts around a nested classifier check so qualified
//! member-extension calls still capture their implicit dispatch receiver, and orders the captured
//! receivers by the body's first use of each, as kotlinc's local declaration lowering does. The
//! scope identity, unlike type or name matching, tells same-typed receiver rungs apart.

use crate::fx_hash::HashMap;

use crate::ast::{Decl, DeclId, ExprId, File, Stmt, StmtId};
use crate::diag::Span;
use crate::fx_hash::HashSet;

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub(super) enum ReceiverUseSite {
    Expression(ExprId),
    Statement(StmtId),
    Declaration(DeclId),
}

/// Source uses belonging to checked construction-site arguments, including their nested bodies.
/// Excluding this set for one anonymous class does not exclude it for a nested class.
#[derive(Default)]
pub(super) struct ReceiverUseSites {
    sites: HashSet<ReceiverUseSite>,
}

impl ReceiverUseSites {
    pub(super) fn anonymous_forwards(
        file: &File,
        declaration: DeclId,
        recorded: &[ExprId],
        selected: Option<&super::ResolvedCtorDelegation>,
    ) -> Self {
        let (Decl::Class(class), Some(selected)) = (file.decl(declaration), selected) else {
            return Self::default();
        };
        let roots = class
            .base_args
            .iter()
            .zip(&selected.argument_slots)
            .filter_map(|(argument, slot)| {
                (selected.vararg != Some(*slot) && recorded.contains(argument)).then_some(*argument)
            })
            .collect::<Vec<_>>();
        Self::construction_arguments(file, &roots)
    }

    fn construction_arguments(file: &File, roots: &[ExprId]) -> Self {
        let mut out = Self::default();
        let mut pending = roots
            .iter()
            .copied()
            .map(ReceiverUseSite::Expression)
            .collect::<Vec<_>>();
        while let Some(site) = pending.pop() {
            if !out.sites.insert(site) {
                continue;
            }
            let mut expressions = Vec::new();
            match site {
                ReceiverUseSite::Expression(expression) => {
                    file.any_child_expr(
                        expression,
                        &mut |child| {
                            expressions.push(child);
                            false
                        },
                        &mut |child| {
                            pending.push(ReceiverUseSite::Statement(child));
                            false
                        },
                    );
                    if let Some(declaration) = file.anonymous_object_classes.get(&expression) {
                        pending.push(ReceiverUseSite::Declaration(*declaration));
                    }
                }
                ReceiverUseSite::Statement(statement) => {
                    file.any_child_stmt(statement, &mut |child| {
                        expressions.push(child);
                        false
                    });
                    if let Stmt::LocalFun(function) = file.stmt(statement) {
                        expressions.extend(
                            function
                                .params
                                .iter()
                                .filter_map(|parameter| parameter.default),
                        );
                    }
                    if let Some(declaration) = file.local_class_decls.get(&statement) {
                        pending.push(ReceiverUseSite::Declaration(*declaration));
                    }
                }
                ReceiverUseSite::Declaration(declaration) => {
                    if let Decl::Class(class) = file.decl(declaration) {
                        expressions.extend(
                            super::capture_storage::local_class_capture_expressions(class),
                        );
                    }
                }
            }
            pending.extend(expressions.into_iter().map(ReceiverUseSite::Expression));
        }
        out
    }

    pub(super) fn contains(&self, site: ReceiverUseSite) -> bool {
        self.sites.contains(&site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_capture_sites_include_nested_anonymous_interface_delegates() {
        let source = "interface I\nopen class Carrier(val value: () -> I)\nfun make(delegate: I) = object : Carrier({ object : I by delegate {} }) {}";
        let mut diagnostics = crate::diag::DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let (nested, delegate) = file
            .anonymous_object_classes
            .values()
            .find_map(|declaration| {
                let Decl::Class(class) = file.decl(*declaration) else {
                    return None;
                };
                class
                    .interface_delegations
                    .first()
                    .map(|delegate| (*declaration, delegate.value))
            })
            .expect("nested delegated anonymous class");
        let argument = file
            .anonymous_object_classes
            .values()
            .find_map(|declaration| {
                let Decl::Class(class) = file.decl(*declaration) else {
                    return None;
                };
                class.base_args.first().copied()
            })
            .expect("outer constructor argument");
        let sites = ReceiverUseSites::construction_arguments(&file, &[argument]);
        assert!(sites.contains(ReceiverUseSite::Declaration(nested)));
        assert!(sites.contains(ReceiverUseSite::Expression(delegate)));
    }
}

/// One use, keyed by the binding's scope identity or, for an extension receiver named through its
/// label or its declaration, by that declaration's span.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiverUse {
    Binding((usize, usize)),
    Extension(Span),
}

#[derive(Default)]
pub(super) struct ReceiverUses {
    counts: HashMap<(usize, usize), usize>,
    order: Vec<(ReceiverUse, ReceiverUseSite)>,
}

impl ReceiverUses {
    pub(super) fn record(&mut self, identity: (usize, usize), site: ReceiverUseSite) {
        *self.counts.entry(identity).or_default() += 1;
        self.order.push((ReceiverUse::Binding(identity), site));
    }

    pub(super) fn record_extension(&mut self, declaration: Span, site: ReceiverUseSite) {
        self.order.push((ReceiverUse::Extension(declaration), site));
    }

    pub(super) fn count(&self, identity: (usize, usize)) -> usize {
        self.counts.get(&identity).copied().unwrap_or_default()
    }

    /// A position to measure later uses from.
    pub(super) fn mark(&self) -> usize {
        self.order.len()
    }

    /// Where the receiver bound at `identity`, or declared as the extension receiver at
    /// `extension`, was first used after `mark`: `usize::MAX` when it was not.
    pub(super) fn first_use_since(
        &self,
        mark: usize,
        identity: (usize, usize),
        extension: Option<Span>,
    ) -> usize {
        self.first_use_since_excluding(mark, identity, extension, &ReceiverUseSites::default())
    }

    pub(super) fn first_use_since_excluding(
        &self,
        mark: usize,
        identity: (usize, usize),
        extension: Option<Span>,
        excluded: &ReceiverUseSites,
    ) -> usize {
        self.order[mark..]
            .iter()
            .position(|(used, site)| {
                !excluded.contains(*site)
                    && match *used {
                        ReceiverUse::Binding(used) => used == identity,
                        ReceiverUse::Extension(used) => Some(used) == extension,
                    }
            })
            .unwrap_or(usize::MAX)
    }
}
