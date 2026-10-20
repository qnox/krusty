//! Diagnostics: spans plus messages, with line/column rendering.

use crate::types::TypeName;

/// A byte range into the source file. `u32` offsets keep this 8 bytes (data-oriented).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Severity {
    Error,
    Warning,
}

/// Stable compiler-warning names exposed through kotlinc's `-Xwarning-level` contract.
///
/// This is separate from [`DiagnosticIdentity`]: a warning name selects command-line policy,
/// while an identity says when two reports describe the same semantic occurrence.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CompilerWarningName {
    DataClassCopyVisibilityWillBeChanged,
    DataClassInvisibleCopyUsage,
    ClassLiteralLhsNotAClass,
    Deprecation,
    DeprecationErrorMigrationPeriodWarning,
    ExplicitTypeArgumentsInPropertyAccess,
    InapplicableOperatorModifier,
    ParenthesizedPackageQualifier,
    TypeIntersectionAsReified,
    TypeIntersectionAsReifiedDeprecation,
    TypeParameterAsReifiedDeprecation,
    TypeArgumentsNotAllowed,
    TypeArgumentsNotAllowedInPackageQualifier,
    WrongNumberOfTypeArgumentsInGetClass,
    ValueClassCannotBeRecursiveViaTypeParameters,
}

impl CompilerWarningName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DataClassCopyVisibilityWillBeChanged => {
                "DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING"
            }
            Self::DataClassInvisibleCopyUsage => "DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING",
            Self::ClassLiteralLhsNotAClass => "CLASS_LITERAL_LHS_NOT_A_CLASS_WARNING",
            Self::Deprecation => "DEPRECATION",
            Self::DeprecationErrorMigrationPeriodWarning => {
                "DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING"
            }
            Self::ExplicitTypeArgumentsInPropertyAccess => {
                "EXPLICIT_TYPE_ARGUMENTS_IN_PROPERTY_ACCESS_WARNING"
            }
            Self::InapplicableOperatorModifier => "INAPPLICABLE_OPERATOR_MODIFIER_WARNING",
            Self::ParenthesizedPackageQualifier => "PARENTHESIZED_PACKAGE_QUALIFIER_WARNING",
            Self::TypeIntersectionAsReified => "TYPE_INTERSECTION_AS_REIFIED_WARNING",
            Self::TypeIntersectionAsReifiedDeprecation => {
                "TYPE_INTERSECTION_AS_REIFIED_DEPRECATION_WARNING"
            }
            Self::TypeParameterAsReifiedDeprecation => {
                "TYPE_PARAMETER_AS_REIFIED_DEPRECATION_WARNING"
            }
            Self::TypeArgumentsNotAllowed => "TYPE_ARGUMENTS_NOT_ALLOWED_WARNING",
            Self::TypeArgumentsNotAllowedInPackageQualifier => {
                "TYPE_ARGUMENTS_NOT_ALLOWED_IN_PACKAGE_QUALIFIER_WARNING"
            }
            Self::WrongNumberOfTypeArgumentsInGetClass => {
                "WRONG_NUMBER_OF_TYPE_ARGUMENTS_IN_GET_CLASS_WARNING"
            }
            Self::ValueClassCannotBeRecursiveViaTypeParameters => {
                "VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING"
            }
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING" => {
                Self::DataClassCopyVisibilityWillBeChanged
            }
            "DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING" => Self::DataClassInvisibleCopyUsage,
            "CLASS_LITERAL_LHS_NOT_A_CLASS_WARNING" => Self::ClassLiteralLhsNotAClass,
            "DEPRECATION" => Self::Deprecation,
            "DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING" => {
                Self::DeprecationErrorMigrationPeriodWarning
            }
            "EXPLICIT_TYPE_ARGUMENTS_IN_PROPERTY_ACCESS_WARNING" => {
                Self::ExplicitTypeArgumentsInPropertyAccess
            }
            "INAPPLICABLE_OPERATOR_MODIFIER_WARNING" => Self::InapplicableOperatorModifier,
            "PARENTHESIZED_PACKAGE_QUALIFIER_WARNING" => Self::ParenthesizedPackageQualifier,
            "TYPE_INTERSECTION_AS_REIFIED_WARNING" => Self::TypeIntersectionAsReified,
            "TYPE_INTERSECTION_AS_REIFIED_DEPRECATION_WARNING" => {
                Self::TypeIntersectionAsReifiedDeprecation
            }
            "TYPE_PARAMETER_AS_REIFIED_DEPRECATION_WARNING" => {
                Self::TypeParameterAsReifiedDeprecation
            }
            "TYPE_ARGUMENTS_NOT_ALLOWED_WARNING" => Self::TypeArgumentsNotAllowed,
            "TYPE_ARGUMENTS_NOT_ALLOWED_IN_PACKAGE_QUALIFIER_WARNING" => {
                Self::TypeArgumentsNotAllowedInPackageQualifier
            }
            "WRONG_NUMBER_OF_TYPE_ARGUMENTS_IN_GET_CLASS_WARNING" => {
                Self::WrongNumberOfTypeArgumentsInGetClass
            }
            "VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING" => {
                Self::ValueClassCannotBeRecursiveViaTypeParameters
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
mod warning_name_tests {
    use super::CompilerWarningName;

    #[test]
    fn every_compiler_warning_name_round_trips_through_its_wire_spelling() {
        let names = [
            CompilerWarningName::DataClassCopyVisibilityWillBeChanged,
            CompilerWarningName::DataClassInvisibleCopyUsage,
            CompilerWarningName::ClassLiteralLhsNotAClass,
            CompilerWarningName::Deprecation,
            CompilerWarningName::DeprecationErrorMigrationPeriodWarning,
            CompilerWarningName::ExplicitTypeArgumentsInPropertyAccess,
            CompilerWarningName::InapplicableOperatorModifier,
            CompilerWarningName::ParenthesizedPackageQualifier,
            CompilerWarningName::TypeIntersectionAsReified,
            CompilerWarningName::TypeIntersectionAsReifiedDeprecation,
            CompilerWarningName::TypeParameterAsReifiedDeprecation,
            CompilerWarningName::TypeArgumentsNotAllowed,
            CompilerWarningName::TypeArgumentsNotAllowedInPackageQualifier,
            CompilerWarningName::WrongNumberOfTypeArgumentsInGetClass,
            CompilerWarningName::ValueClassCannotBeRecursiveViaTypeParameters,
        ];
        assert_eq!(
            names.map(|name| CompilerWarningName::parse(name.as_str())),
            names.map(Some)
        );
        assert_eq!(CompilerWarningName::parse("UNKNOWN_WARNING"), None);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum DiagnosticKind {
    #[default]
    Compiler,
    IncompatibleEquality,
    ValReassignment,
    Inspection,
    /// An `expect`/`actual` actualization error. The reference compiler actualizes after its
    /// frontend has reported, so these follow every frontend diagnostic in its ledger, in the order
    /// actualization produced them rather than in source order.
    Actualization,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DiagnosticIdentity {
    ClassifierAccess {
        reference: Span,
        classifier: TypeName,
    },
    /// Resolution failure for one import occurrence. Pass 2 may enter the same file once per
    /// declaration group; the source occurrence, not its rendered message, identifies the one
    /// file-level diagnostic those entries share.
    ImportResolution { reference: Span },
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Compiler-facing source range.
    pub span: Span,
    /// Optional editor-facing source range.
    pub editor_span: Option<Span>,
    pub severity: Severity,
    /// The `-Xwarning-level` identity of a warning, when kotlinc exposes one.
    pub warning_name: Option<CompilerWarningName>,
    pub kind: DiagnosticKind,
    pub msg: String,
    pub identity: Option<DiagnosticIdentity>,
    /// Index of the source file this diagnostic belongs to (into the driver's `files`/`sources`
    /// lists). Diagnostics are produced one file at a time, so the sink stamps each with the file
    /// currently being processed — without it, a multi-file compile renders every error against the
    /// wrong file's source (bogus line numbers, foreign types). Single-file callers leave it 0.
    pub file: u32,
}

#[derive(Default)]
pub struct DiagSink {
    pub diags: Vec<Diagnostic>,
    /// The file index stamped onto subsequent diagnostics. The driver/front-end sets this before
    /// processing each file (see `set_file`); it stays 0 for the single-file box/test harness.
    current_file: u32,
    /// When present, this semantic operation owns diagnostics only inside these source ranges.
    /// Pass-1 retained-fragment preparation uses this while traversing lexical containers whose
    /// ordinary expressions are authoritative only in Pass 2.
    authoritative_ranges: Option<Vec<Span>>,
    /// Warnings about the compilation's configuration rather than a source position, such as an
    /// unresolved `-opt-in` marker. Each message is kept once, in first-report order.
    pub module_warnings: Vec<String>,
}

impl DiagSink {
    pub fn new() -> DiagSink {
        DiagSink {
            diags: Vec::new(),
            current_file: 0,
            authoritative_ranges: None,
            module_warnings: Vec::new(),
        }
    }

    /// Report a warning about the module's configuration, once however many units report it.
    pub fn module_warning(&mut self, message: impl Into<String>) {
        let message = message.into();
        if !self.module_warnings.contains(&message) {
            self.module_warnings.push(message);
        }
    }

    fn accepts(&self, span: Span) -> bool {
        self.authoritative_ranges.as_ref().is_none_or(|ranges| {
            ranges
                .iter()
                .any(|range| range.lo <= span.lo && span.hi <= range.hi)
        })
    }

    /// Run one semantic operation with diagnostic ownership restricted to `ranges`.
    ///
    /// This is not error recovery: diagnostics outside the ranges belong to another phase and are
    /// never inserted. Nested ownership scopes intersect naturally because an inner operation may
    /// only narrow the outer operation's ranges.
    pub(crate) fn with_authoritative_ranges<R>(
        &mut self,
        ranges: &[Span],
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prior = self.authoritative_ranges.take();
        let selected = match prior.as_ref() {
            Some(outer) => outer
                .iter()
                .flat_map(|candidate| {
                    ranges.iter().filter_map(|range| {
                        let lo = candidate.lo.max(range.lo);
                        let hi = candidate.hi.min(range.hi);
                        (lo <= hi).then_some(Span::new(lo, hi))
                    })
                })
                .collect(),
            None => ranges.to_vec(),
        };
        self.authoritative_ranges = Some(selected);
        let result = operation(self);
        self.authoritative_ranges = prior;
        result
    }

    /// Stamp subsequent diagnostics as belonging to file `index` (the front-end calls this at the
    /// start of each per-file pass so errors carry their true origin in a multi-file compile).
    pub fn set_file(&mut self, index: u32) {
        self.current_file = index;
    }

    pub fn current_file(&self) -> u32 {
        self.current_file
    }

    pub fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.error_kind(span, DiagnosticKind::Compiler, msg);
    }

    /// Report a diagnostic that does not fail the compilation.
    pub fn warning(&mut self, span: Span, msg: impl Into<String>) {
        self.push_warning(span, None, msg.into());
    }

    /// Report a warning whose stable name can be configured through `-Xwarning-level`.
    pub fn warning_named(&mut self, span: Span, name: CompilerWarningName, msg: impl Into<String>) {
        self.push_warning(span, Some(name), msg.into());
    }

    fn push_warning(&mut self, span: Span, name: Option<CompilerWarningName>, msg: String) {
        if !self.accepts(span) {
            return;
        }
        self.diags.push(Diagnostic {
            span,
            editor_span: None,
            severity: Severity::Warning,
            warning_name: name,
            kind: DiagnosticKind::Compiler,
            msg,
            identity: None,
            file: self.current_file,
        });
    }

    pub fn error_kind(&mut self, span: Span, kind: DiagnosticKind, msg: impl Into<String>) {
        if !self.accepts(span) {
            return;
        }
        let msg = msg.into();
        crate::trace_compiler!(
            "diagnostic",
            "file={} span={span:?} kind={kind:?} message={msg}",
            self.current_file,
        );
        self.diags.push(Diagnostic {
            span,
            editor_span: None,
            severity: Severity::Error,
            warning_name: None,
            kind,
            msg,
            identity: None,
            file: self.current_file,
        });
    }

    pub fn error_with_identity(
        &mut self,
        span: Span,
        identity: DiagnosticIdentity,
        msg: impl Into<String>,
    ) {
        if !self.accepts(span) {
            return;
        }
        let msg = msg.into();
        crate::trace_compiler!(
            "diagnostic",
            "file={} span={span:?} kind={:?} identity={identity:?} message={msg}",
            self.current_file,
            DiagnosticKind::Compiler,
        );
        self.diags.push(Diagnostic {
            span,
            editor_span: None,
            severity: Severity::Error,
            warning_name: None,
            kind: DiagnosticKind::Compiler,
            msg,
            identity: Some(identity),
            file: self.current_file,
        });
    }

    pub fn error_with_editor_span(
        &mut self,
        span: Span,
        editor_span: Span,
        msg: impl Into<String>,
    ) {
        if !self.accepts(span) {
            return;
        }
        let msg = msg.into();
        crate::trace_compiler!(
            "diagnostic",
            "file={} span={span:?} editor_span={editor_span:?} kind={:?} message={msg}",
            self.current_file,
            DiagnosticKind::Compiler,
        );
        self.diags.push(Diagnostic {
            span,
            editor_span: Some(editor_span),
            severity: Severity::Error,
            warning_name: None,
            kind: DiagnosticKind::Compiler,
            msg,
            identity: None,
            file: self.current_file,
        });
    }

    pub fn collapse_duplicates(&mut self) {
        self.collapse_duplicates_from(0);
    }

    pub(crate) fn collapse_duplicates_from(&mut self, start: usize) {
        let start = start.min(self.diags.len());
        let mut tail = self.diags.split_off(start);
        let mut seen =
            crate::fx_hash::HashSet::with_capacity_and_hasher(tail.len(), Default::default());
        let mut seen_identities = crate::fx_hash::HashSet::default();
        tail.retain(|diagnostic| {
            if let Some(identity) = diagnostic.identity {
                seen_identities.insert((
                    diagnostic.file,
                    diagnostic.severity,
                    diagnostic.warning_name,
                    diagnostic.kind,
                    identity,
                ))
            } else {
                seen.insert((
                    diagnostic.file,
                    diagnostic.span,
                    diagnostic.editor_span,
                    diagnostic.severity,
                    diagnostic.warning_name,
                    diagnostic.kind,
                    diagnostic.msg.clone(),
                ))
            }
        });
        self.diags.extend(tail);
    }

    /// Order the diagnostics from `start` by where they start, so one call's diagnostics read as
    /// kotlinc lists them whichever check found them first. The stable sort keeps production order
    /// for diagnostics that start at the same offset.
    pub(crate) fn sort_source_order_from(&mut self, start: usize) {
        let start = start.min(self.diags.len());
        self.diags[start..].sort_by_key(|diagnostic| (diagnostic.file, diagnostic.span.lo));
    }

    /// Restore deterministic source order after diagnostics from multiple semantic phases have
    /// been merged. The stable sort preserves production order for diagnostics at the same range.
    /// Actualization diagnostics keep their production order after all others, as the reference
    /// compiler reports them from a later phase.
    pub(crate) fn sort_source_order(&mut self) {
        self.diags.sort_by_key(|diagnostic| match diagnostic.kind {
            DiagnosticKind::Actualization => (1, 0, 0, 0),
            _ => (0, diagnostic.file, diagnostic.span.lo, diagnostic.span.hi),
        });
    }

    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// Render `path:line:col: severity: msg` lines against the original source. Single-file callers:
    /// every diagnostic is rendered against `src` (its file index is assumed 0).
    pub fn render(&self, path: &str, src: &str) -> String {
        let mut out = String::new();
        for d in &self.diags {
            out.push_str(&self.render_one(d, path, src));
        }
        out
    }

    /// Render every diagnostic against ITS OWN source file (by `Diagnostic::file`), once. `files` is
    /// the driver's parallel `(path, source)` list. A diagnostic whose file index is out of range
    /// (defensive) falls back to the first file. This is the multi-file-correct renderer.
    pub fn render_all(&self, files: &[(&str, &str)]) -> String {
        let mut out = String::new();
        for warning in &self.module_warnings {
            out.push_str("warning: ");
            out.push_str(warning);
            out.push('\n');
        }
        for d in &self.diags {
            let (path, src) = files
                .get(d.file as usize)
                .copied()
                .or_else(|| files.first().copied())
                .unwrap_or(("<unknown>", ""));
            out.push_str(&self.render_one(d, path, src));
        }
        out
    }

    fn render_one(&self, d: &Diagnostic, path: &str, src: &str) -> String {
        let (line, col) = line_col(src, d.span.lo);
        let sev = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        format!("{path}:{line}:{col}: {sev}: {}\n", d.msg)
    }
}

/// 1-based line and column for a byte offset.
pub fn line_col(src: &str, offset: u32) -> (usize, usize) {
    let off = (offset as usize).min(src.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, b) in src.bytes().enumerate() {
        if i >= off {
            break;
        }
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_basic() {
        let src = "ab\ncde\nf";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 1), (1, 2));
        assert_eq!(line_col(src, 3), (2, 1)); // 'c'
        assert_eq!(line_col(src, 7), (3, 1)); // 'f'
    }

    #[test]
    fn render_includes_location() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        let r = s.render("X.kt", "ab\ncde");
        assert!(r.contains("X.kt:2:1: error: boom"), "got: {r}");
        assert!(s.has_errors());
    }

    #[test]
    fn identical_diagnostics_collapse() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        s.error(Span::new(3, 4), "boom");
        s.error(Span::new(5, 6), "boom");
        s.error(Span::new(3, 4), "other");
        s.set_file(1);
        s.error(Span::new(3, 4), "boom");

        s.collapse_duplicates();

        assert_eq!(s.diags.len(), 4, "{:?}", s.diags);
    }

    #[test]
    fn warning_policy_name_is_part_of_duplicate_identity() {
        let mut s = DiagSink::new();
        let span = Span::new(3, 4);
        s.warning(span, "same rendering");
        s.warning_named(span, CompilerWarningName::Deprecation, "same rendering");

        s.collapse_duplicates();

        assert_eq!(s.diags.len(), 2, "{:?}", s.diags);
        assert_eq!(s.diags[0].warning_name, None);
        assert_eq!(
            s.diags[1].warning_name,
            Some(CompilerWarningName::Deprecation)
        );
    }

    #[test]
    fn semantic_identity_collapse_preserves_first_spelling() {
        let mut s = DiagSink::new();
        let identity = DiagnosticIdentity::ClassifierAccess {
            reference: Span::new(3, 4),
            classifier: crate::types::type_name("scope/Classifier"),
        };
        s.error_with_identity(Span::new(3, 4), identity, "cannot access 'Alias'");
        s.error_with_identity(
            Span::new(3, 4),
            identity,
            "cannot access 'scope.Classifier'",
        );
        s.error_with_identity(
            Span::new(5, 6),
            DiagnosticIdentity::ClassifierAccess {
                reference: Span::new(5, 6),
                classifier: crate::types::type_name("scope/Classifier"),
            },
            "cannot access 'SecondAlias'",
        );

        s.collapse_duplicates();

        assert_eq!(s.diags.len(), 2);
        assert_eq!(s.diags[0].msg, "cannot access 'Alias'");
        assert_eq!(s.diags[1].msg, "cannot access 'SecondAlias'");
    }

    #[test]
    fn ranged_collapse_preserves_existing_diagnostics() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        s.error(Span::new(3, 4), "boom");
        let start = s.diags.len();
        s.error(Span::new(5, 6), "later");
        s.error(Span::new(5, 6), "later");

        s.collapse_duplicates_from(start);

        assert_eq!(s.diags.len(), 3);
    }

    #[test]
    fn render_all_attributes_each_diag_to_its_own_file() {
        // Two files; an error in each. `render_all` must render each against ITS OWN source — not the
        // whole list against every file (the multi-file mis-attribution bug).
        let mut s = DiagSink::new();
        s.set_file(0);
        s.error(Span::new(0, 1), "in A"); // offset 0 → A.kt:1:1
        s.set_file(1);
        s.error(Span::new(4, 5), "in B"); // offset 4 → line 2 of B's source
        let files = [("A.kt", "xyz"), ("B.kt", "ab\ncde")];
        let r = s.render_all(&files);
        assert!(r.contains("A.kt:1:1: error: in A"), "got: {r}");
        assert!(r.contains("B.kt:2:2: error: in B"), "got: {r}");
        // Exactly two lines — no duplication across files.
        assert_eq!(r.lines().count(), 2, "got: {r}");
    }
}
