//! What each function's source declaration says about its parameters beyond their IR types, by
//! [`FunId`].
//!
//! A declaration's parameter facts are one value: when a pass copies, moves or reshapes a function's
//! parameters, it does so to the whole value, so a parameter fact cannot be left behind by a site
//! that names only some of them. The value lives in a column parallel to [`IrFile::functions`]; a
//! function without one has the empty value.

use super::{DeclarationAnnotations, FunId, IrEqualityBoundParameter, IrFile, IrVarargParameter};

/// The declared parameter facts of one function. Each list is parallel to the function's physical
/// parameters ([`super::IrFunction::params`]) or empty, which means no parameter has the fact.
#[derive(Clone, Debug, Default)]
pub struct IrDeclaredParameters {
    /// Whether each parameter was declared nullable (`a: String?`). The IR parameter type stays
    /// non-null on purpose: it feeds the value-class name mangle, which must not see the declared
    /// nullability (a nullable value-class parameter mangles like a non-null one). `@Metadata` and
    /// the `@NotNull`/`@Nullable` parameter annotations read it here instead.
    pub nullable: Vec<bool>,
    /// The user annotations on each parameter (an extension's leading receiver slot is present and
    /// empty). Retention stays semantic here; the JVM split into visible and invisible attributes
    /// belongs to the emitter.
    pub annotations: Vec<DeclarationAnnotations>,
    /// Whether each parameter carries Kotlin's semantic `@NoInfer` type-use marker. An extension
    /// receiver occupies its slot with `false`; metadata projection removes that slot again. This is
    /// inference policy, not a JVM fact.
    pub no_infer: Vec<bool>,
    /// The `vararg` parameter. Class `@Metadata` must record its element type, or a consumer demands
    /// one literal array.
    pub vararg: Option<IrVarargParameter>,
    /// The parameter an `equals` reads as its equality bound, declared with `@EqualityBound` or
    /// inherited from the `equals` it overrides. `@Metadata` records it, or a consuming module's
    /// override loses the bound.
    pub equality_bound: Option<IrEqualityBoundParameter>,
    /// How many of the leading parameters are context parameters. `@Metadata` records them as
    /// `Function.context_parameter`, or a consuming compiler demands them positionally.
    pub context_count: usize,
}

static NO_DECLARED_PARAMETERS: IrDeclaredParameters = IrDeclaredParameters {
    nullable: Vec::new(),
    annotations: Vec::new(),
    no_infer: Vec::new(),
    vararg: None,
    equality_bound: None,
    context_count: 0,
};

impl IrDeclaredParameters {
    /// Whether parameter `index` was declared nullable.
    pub fn is_nullable(&self, index: usize) -> bool {
        self.nullable.get(index).copied().unwrap_or(false)
    }

    /// Whether parameter `index` carries `@NoInfer`.
    pub fn is_no_infer(&self, index: usize) -> bool {
        self.no_infer.get(index).copied().unwrap_or(false)
    }

    /// The annotations on parameter `index`, if any were recorded.
    pub fn annotations_of(&self, index: usize) -> Option<&DeclarationAnnotations> {
        self.annotations.get(index)
    }

    /// The lengths of the per-parameter lists that are recorded (non-empty).
    pub(crate) fn recorded_lengths(&self) -> impl Iterator<Item = usize> {
        [
            self.nullable.len(),
            self.annotations.len(),
            self.no_infer.len(),
        ]
        .into_iter()
        .filter(|&len| len != 0)
    }

    /// The facts of `count` parameters starting at `start`: the recorded values, or the defaults
    /// for a list that is not recorded.
    pub(crate) fn parameter_range(&self, start: usize, count: usize) -> IrParameterFacts {
        fn range<T: Clone + Default>(values: &[T], start: usize, count: usize) -> Vec<T> {
            if values.is_empty() {
                vec![T::default(); count]
            } else {
                values[start..start + count].to_vec()
            }
        }
        IrParameterFacts {
            nullable: range(&self.nullable, start, count),
            annotations: range(&self.annotations, start, count),
            no_infer: range(&self.no_infer, start, count),
        }
    }

    /// Replace parameter `parameter` of every recorded list with `replacement`'s values, for each
    /// `(parameter, replacement)` pair. The pairs are in ascending parameter order, and every
    /// recorded list must be parallel to the `old_arity` parameters the function had before.
    pub(crate) fn splice_parameters(
        &mut self,
        replacements: &[(usize, IrParameterFacts)],
        old_arity: usize,
    ) {
        fn splice<T: Clone>(
            values: &mut Vec<T>,
            replacements: &[(usize, IrParameterFacts)],
            old_arity: usize,
            facts: impl Fn(&IrParameterFacts) -> &[T],
        ) {
            if values.is_empty() {
                return;
            }
            assert_eq!(
                values.len(),
                old_arity,
                "a declared parameter fact must be exactly parallel to the pre-specialization \
                 parameter shape"
            );
            for (parameter, replacement) in replacements.iter().rev() {
                values.splice(*parameter..=*parameter, facts(replacement).iter().cloned());
            }
        }
        splice(&mut self.nullable, replacements, old_arity, |facts| {
            &facts.nullable
        });
        splice(&mut self.annotations, replacements, old_arity, |facts| {
            &facts.annotations
        });
        splice(&mut self.no_infer, replacements, old_arity, |facts| {
            &facts.no_infer
        });
    }
}

/// The per-parameter facts of a run of parameters, cut from one function's [`IrDeclaredParameters`]
/// to be spliced into another's.
#[derive(Clone, Debug, Default)]
pub(crate) struct IrParameterFacts {
    nullable: Vec<bool>,
    annotations: Vec<DeclarationAnnotations>,
    no_infer: Vec<bool>,
}

/// The declared parameter facts of every function, by [`FunId`].
#[derive(Default)]
pub(crate) struct IrDeclaredParameterColumn {
    functions: Vec<IrDeclaredParameters>,
}

impl IrDeclaredParameterColumn {
    fn get(&self, function: FunId) -> &IrDeclaredParameters {
        self.functions
            .get(function as usize)
            .unwrap_or(&NO_DECLARED_PARAMETERS)
    }

    fn get_mut(&mut self, function: FunId) -> &mut IrDeclaredParameters {
        let index = function as usize;
        if self.functions.len() <= index {
            self.functions
                .resize_with(index + 1, IrDeclaredParameters::default);
        }
        &mut self.functions[index]
    }
}

impl IrFile {
    /// The declared parameter facts of `function`.
    pub fn declared_parameters(&self, function: FunId) -> &IrDeclaredParameters {
        self.declared_parameters.get(function)
    }

    /// The declared parameter facts of `function`, to record or change them.
    pub fn declared_parameters_mut(&mut self, function: FunId) -> &mut IrDeclaredParameters {
        self.declared_parameters.get_mut(function)
    }

    /// Every function's declared parameter facts, to rewrite something all of them mention.
    pub(crate) fn all_declared_parameters_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut IrDeclaredParameters> {
        self.declared_parameters.functions.iter_mut()
    }

    /// Give `target` the declared parameter facts `source` has.
    pub(crate) fn copy_declared_parameters(&mut self, source: FunId, target: FunId) {
        let parameters = self.declared_parameters(source).clone();
        *self.declared_parameters_mut(target) = parameters;
    }

    /// Take `function`'s declared parameter facts, leaving it with none.
    pub fn take_declared_parameters(&mut self, function: FunId) -> IrDeclaredParameters {
        std::mem::take(self.declared_parameters_mut(function))
    }

    /// Forget the declared parameter facts of every function from `first` on, which a rollback
    /// discards.
    pub(crate) fn truncate_declared_parameters(&mut self, first: FunId) {
        self.declared_parameters.functions.truncate(first as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::IrDeclaredParameters;
    use crate::ir::{DeclarationAnnotations, IrFile, IrVarargParameter};

    /// The facts compared by value: every list's contents (annotations by whether each slot
    /// declares any), the vararg and the context count.
    fn facts(
        parameters: &IrDeclaredParameters,
    ) -> (
        Vec<bool>,
        Vec<bool>,
        Vec<bool>,
        Option<IrVarargParameter>,
        usize,
    ) {
        (
            parameters.nullable.clone(),
            parameters
                .annotations
                .iter()
                .map(DeclarationAnnotations::declares_annotations)
                .collect(),
            parameters.no_infer.clone(),
            parameters.vararg,
            parameters.context_count,
        )
    }

    fn none() -> (
        Vec<bool>,
        Vec<bool>,
        Vec<bool>,
        Option<IrVarargParameter>,
        usize,
    ) {
        facts(&IrDeclaredParameters::default())
    }

    #[test]
    fn a_function_without_recorded_facts_has_the_empty_value() {
        let mut ir = IrFile::default();
        ir.declared_parameters_mut(2).context_count = 1;

        assert_eq!(facts(ir.declared_parameters(0)), none());
        assert_eq!(facts(ir.declared_parameters(7)), none());
        assert_eq!(ir.declared_parameters(2).context_count, 1);
        assert!(!ir.declared_parameters(2).is_nullable(0));
    }

    #[test]
    fn a_copied_function_takes_its_source_facts_whole() {
        let mut ir = IrFile::default();
        let source = IrDeclaredParameters {
            nullable: vec![true, false],
            annotations: vec![
                DeclarationAnnotations::default(),
                DeclarationAnnotations::default().with_suppressed_source_application(true),
            ],
            no_infer: vec![false, true],
            vararg: Some(IrVarargParameter {
                index: 1,
                is_last: true,
            }),
            equality_bound: None,
            context_count: 1,
        };
        *ir.declared_parameters_mut(0) = source.clone();
        ir.declared_parameters_mut(1).nullable = vec![true];

        ir.copy_declared_parameters(0, 1);

        assert_eq!(facts(ir.declared_parameters(1)), facts(&source));
        assert!(ir.declared_parameters(1).annotations[1].has_suppressed_source_application());
    }

    #[test]
    fn a_moved_function_leaves_its_source_without_facts() {
        let mut ir = IrFile::default();
        ir.declared_parameters_mut(0).no_infer = vec![true];

        let taken = ir.take_declared_parameters(0);
        *ir.declared_parameters_mut(1) = taken;

        assert_eq!(facts(ir.declared_parameters(0)), none());
        assert_eq!(ir.declared_parameters(1).no_infer, [true]);
    }

    #[test]
    fn splicing_replaces_one_parameter_in_every_recorded_list() {
        let source = IrDeclaredParameters {
            nullable: vec![true, true],
            no_infer: vec![false, true],
            ..IrDeclaredParameters::default()
        };
        let mut target = IrDeclaredParameters {
            nullable: vec![false, false, true],
            annotations: vec![DeclarationAnnotations::default(); 3],
            ..IrDeclaredParameters::default()
        };

        target.splice_parameters(&[(1, source.parameter_range(0, 2))], 3);

        assert_eq!(target.nullable, [false, true, true, true]);
        assert_eq!(target.annotations.len(), 4);
        assert!(target.no_infer.is_empty());
    }

    #[test]
    fn a_range_of_an_unrecorded_list_has_default_facts() {
        let parameters = IrDeclaredParameters {
            no_infer: vec![false, true, true],
            ..IrDeclaredParameters::default()
        };

        let range = parameters.parameter_range(1, 2);

        assert_eq!(range.nullable, [false, false]);
        assert_eq!(range.annotations.len(), 2);
        assert!(!range
            .annotations
            .iter()
            .any(DeclarationAnnotations::declares_annotations));
        assert_eq!(range.no_infer, [true, true]);
    }
}
