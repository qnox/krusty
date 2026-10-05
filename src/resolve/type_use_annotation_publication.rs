//! Checked type-use annotation folding and its stable metadata publication.
//!
//! Pass 1 binds each type-use annotation's classifier while declaration spellings are collected,
//! but an application's arguments are constant expressions (`@Bin(LIMIT)`, `@Moded(Mode.On,
//! Item::class)`) the checker can fold only once signatures and constants are final. This bounded
//! fragment folds every application in the lexical scope of the declaration that contains it, then
//! seals each published spelling: a bound occurrence becomes its checked application, and a
//! `SOURCE`-retained one leaves the record.

use super::*;
use crate::spelling::{AnnotationOccurrence, CheckedTypeUseAnnotations};

/// Fold every type-use annotation application of `files` and seal the spellings Pass 1 publishes.
pub(crate) fn publish_checked_type_use_annotations(
    files: &[File],
    index: &mut crate::fir::ResolvedModuleIndex,
    table: &mut SymbolTable,
    diags: &mut DiagSink,
) {
    let mut checked = CheckedTypeUseAnnotations::new();
    for (file_index, file) in files.iter().enumerate() {
        let file_index = file_index as u32;
        let applications = file
            .type_annotations
            .values()
            .flatten()
            .filter(|annotation| table.resolved_annotation(file_index, annotation).is_some())
            .collect::<Vec<_>>();
        if applications.is_empty() {
            continue;
        }
        // This fragment re-enters declaration headers that Pass 2 checks again; only the
        // applications it owns may report here.
        let owned_ranges = applications
            .iter()
            .map(|annotation| application_range(file, annotation))
            .collect::<Vec<_>>();
        let selected = file
            .decls
            .iter()
            .map(|&declaration| declaration_span(file, declaration))
            .collect::<std::collections::HashSet<_>>();
        let no_bodies = std::collections::HashSet::new();
        diags.set_file(file_index);
        let info = diags.with_authoritative_ranges(&owned_ranges, |diags| {
            check_file_at_impl_mode_with_index(
                file,
                file_index,
                Some(files),
                table,
                None,
                diags,
                CaptureDiscovery::Published,
                Some(&selected),
                Some(&no_bodies),
                None,
                None,
                None,
                SourceFragmentMode::TypeUseAnnotations,
                None,
            )
        });
        for annotation in applications {
            let Some(applied) = info.applied_annotation(annotation) else {
                continue;
            };
            let recorded =
                (applied.retention != crate::types::AnnotationRetention::Source).then(|| {
                    crate::types::ResolvedAnnotation {
                        annotation: applied.internal,
                        arguments: applied.values.clone(),
                        facts: applied.facts,
                    }
                });
            let occurrence = AnnotationOccurrence {
                source: file_index,
                span: annotation.span,
            };
            checked.insert(occurrence, recorded);
        }
    }
    let mut unchecked = Vec::new();
    for spellings in table.stable_declared_spellings.values_mut() {
        spellings.seal(&checked, &mut unchecked);
    }
    for (spelling, _, _) in table.alias_expansion_spellings.values_mut() {
        spelling.seal(&checked, &mut unchecked);
    }
    for spelling in index.type_alias_spellings_mut() {
        spelling.seal(&checked, &mut unchecked);
    }
    // A missing value is an application the checker rejected, which it reported. Without such a
    // report the fragment failed to reach an application it was required to fold.
    if !diags.has_errors() {
        for occurrence in unchecked {
            diags.set_file(occurrence.source);
            diags.error(
                occurrence.span,
                "internal error: a type-use annotation application was not checked",
            );
        }
    }
}

fn declaration_span(file: &File, declaration: DeclId) -> Span {
    match file.decl(declaration) {
        Decl::Fun(function) => function.span,
        Decl::Class(class) => class.span,
        Decl::Property(property) => property.span,
    }
}

/// The source range of one application: its classifier reference through its last argument.
fn application_range(file: &File, annotation: &AnnotationRef) -> Span {
    let hi = application_arguments(file, annotation)
        .iter()
        .filter_map(|&argument| file.expr_span(argument))
        .map(|span| span.hi)
        .fold(annotation.span.hi, u32::max);
    Span::new(annotation.span.lo, hi)
}

fn application_arguments<'f>(file: &'f File, annotation: &AnnotationRef) -> &'f [ExprId] {
    file.type_annotation_arguments
        .get(&(annotation.span.lo, annotation.span.hi))
        .map_or(&[], Vec::as_slice)
}

impl Checker<'_> {
    /// Check the type-use annotation applications whose innermost enclosing checked declaration is
    /// `owner` (`None`: the file scope, which owns top-level typealiases), in `scope`.
    ///
    /// An argument is a constant expression: it may name a constant, an enum entry, or a class of
    /// the enclosing classifiers, but never a value parameter or a type parameter, so a member's
    /// applications fold on its classifier's rung.
    pub(super) fn check_owned_type_use_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        owner: Option<DeclId>,
    ) {
        let mut occurrences = self
            .file
            .type_annotations
            .iter()
            .filter(|(&offset, _)| self.type_use_annotation_owner(offset) == owner)
            .map(|(&offset, annotations)| (offset, annotations.clone()))
            .collect::<Vec<_>>();
        occurrences.sort_by_key(|(offset, _)| *offset);
        let file = self.file;
        for annotation in occurrences
            .into_iter()
            .flat_map(|(_, annotations)| annotations)
        {
            if !self
                .bound_annotation_identities
                .contains_key(&(annotation.span.lo, annotation.span.hi))
            {
                continue;
            }
            let arguments = application_arguments(file, &annotation);
            self.check_annotation_application(scope, &annotation, arguments);
            let key = (annotation.span.lo, annotation.span.hi);
            if let Some(mut applied) = self.applied_annotations.remove(&key) {
                self.order_as_written(applied.internal, &mut applied.values, arguments, None);
                self.applied_annotations.insert(key, applied);
            }
        }
    }

    /// Put folded argument values in the order the source wrote them, at every nesting depth.
    ///
    /// The checker folds an application in DECLARATION order, which is the order kotlinc writes
    /// a class-file annotation attribute in. `@Metadata` records a type-use application's
    /// arguments as written instead: `@Moded(kind = Holder::class, mode = Mode.Off)` lists `kind`
    /// first. An element the source left out (an omitted vararg's `[]`) follows the written ones.
    fn order_as_written(
        &self,
        internal: TypeName,
        values: &mut [(String, crate::types::AnnotationValue)],
        arguments: &[ExprId],
        nested_call: Option<ExprId>,
    ) {
        let Some((elements, parameters, _)) = self.annotation_shape(internal) else {
            return;
        };
        let names = self.annotation_argument_names(arguments, nested_call);
        let Ok(indices) =
            self.annotation_argument_parameter_indices(&parameters, arguments, &names)
        else {
            return;
        };
        let mut written = Vec::<&str>::new();
        for (&argument, &index) in arguments.iter().zip(&indices) {
            let Some((name, _)) = elements.get(index) else {
                return;
            };
            if !written.contains(&name.as_str()) {
                written.push(name);
            }
            if let Some((_, value)) = values.iter_mut().find(|(element, _)| element == name) {
                self.order_nested_as_written(value, argument);
            }
        }
        values.sort_by_key(|(name, _)| {
            written
                .iter()
                .position(|written| written == name)
                .unwrap_or(written.len())
        });
    }

    /// [`Self::order_as_written`] inside one argument: a nested annotation, or the nested
    /// annotations of an array argument.
    fn order_nested_as_written(&self, value: &mut crate::types::AnnotationValue, argument: ExprId) {
        use crate::types::AnnotationValue;
        match (value, self.file.expr(argument)) {
            (AnnotationValue::Annotation { internal, values }, Expr::Call { args, .. }) => {
                let args = args.clone();
                self.order_as_written(*internal, values, &args, Some(argument));
            }
            (AnnotationValue::Array(values), Expr::AnnotationArrayLiteral(elements))
            | (AnnotationValue::Array(values), Expr::Call { args: elements, .. })
                if values.len() == elements.len() =>
            {
                let elements = elements.clone();
                for (value, element) in values.iter_mut().zip(elements) {
                    self.order_nested_as_written(value, element);
                }
            }
            _ => {}
        }
    }

    /// The smallest file-level declaration this checker enters that contains `offset`. Local and
    /// anonymous classifiers are entered only through their enclosing body, so their occurrences
    /// belong to the enclosing declaration.
    fn type_use_annotation_owner(&self, offset: u32) -> Option<DeclId> {
        self.file
            .decls
            .iter()
            .copied()
            .filter(|&declaration| {
                !self.file.is_local_declaration(declaration)
                    && !self
                        .anonymous_lexical_scope
                        .belongs_to_anonymous_subtree(declaration)
            })
            .map(|declaration| (declaration, declaration_span(self.file, declaration)))
            .filter(|(_, span)| span.lo <= offset && offset < span.hi)
            .min_by_key(|(_, span)| span.hi - span.lo)
            .map(|(declaration, _)| declaration)
    }
}
