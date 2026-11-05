//! The sequences kotlinc numbers the local callables it lifts in.

/// One sequence of lifted local callables: the source file that declares it, the lexical owner,
/// and the outermost declaration name, as a [`crate::fir::FirLiftingSite`] spells them.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct IrLiftingSequence {
    pub source: crate::fir::SourceFileId,
    pub owner: Box<str>,
    pub container: Box<str>,
}

/// The outermost position of the path a callable was lifted along: the declaration whose body it
/// is written in, through every enclosing local callable. A callable that stops being lifted (a
/// lambda realized as a class of its own) keeps it, so a target can still find that declaration's
/// realization once its own passes have placed it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IrLiftingRoot {
    pub sequence: IrLiftingSequence,
    pub position: u32,
}

/// One position of an [`IrLiftingSequence`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IrLiftingEntry {
    pub kind: crate::lifting_provenance::LiftingCallableKind,
    pub name: Option<Box<str>>,
    pub lifted: bool,
    /// The position of the innermost lambda this entry is nested in, whose own numbering it takes
    /// part in; `None` for one numbered by the sequence itself.
    pub scope: Option<u32>,
    /// The declaration whose body writes this entry. Overloads that share the sequence's source
    /// name are distinct containers; a target whose ABI renames one of them names and numbers its
    /// lifted callables after that realization.
    pub container: Option<super::IrEnclosure>,
    /// Source order used to assign `$lambda$N`. A Java SAM adapter is not a source callable, so it
    /// takes an order between the value it adapts and the next source callable; `position` stays
    /// the stable key the frontend recorded.
    pub order: u32,
}

impl super::IrFile {
    /// Place `implementation`, a lambda a compiler plugin generates in a declaration's body, as the
    /// last callable lifted out of that declaration's sequence, so a target names it as it names
    /// a source lambda written after the others there (kotlinc's IR plugins run before its
    /// lowering lifts lambdas). The caller makes sure every callable already in the sequence is
    /// one of the declaration's own lambdas, numbered directly by the sequence.
    pub(crate) fn place_generated_lambda(
        &mut self,
        sequence: IrLiftingSequence,
        source_order: u32,
        container: super::IrEnclosure,
        implementation: super::FunId,
    ) {
        let entries = self.lifting_sequences.entry(sequence.clone()).or_default();
        assert!(
            entries.values().all(|entry| entry.scope.is_none()),
            "a generated lambda is placed after the lambdas a sequence numbers itself"
        );
        let position = entries.keys().max().map_or(0, |last| last + 1);
        let order = entries
            .values()
            .map(|entry| entry.order + 1)
            .max()
            .unwrap_or(0);
        entries.insert(
            position,
            IrLiftingEntry {
                kind: crate::lifting_provenance::LiftingCallableKind::Lambda,
                name: None,
                lifted: true,
                scope: None,
                container: Some(container),
                order,
            },
        );
        self.lifting_sequence_source_order
            .entry(sequence.clone())
            .or_insert(source_order);
        self.lifted_functions.insert(
            implementation,
            (
                sequence.clone(),
                crate::fir::FirLiftingSite {
                    owner: sequence.owner,
                    container: sequence.container,
                    path: Box::new([crate::fir::FirLiftingStep {
                        kind: crate::lifting_provenance::LiftingCallableKind::Lambda,
                        name: None,
                        position,
                    }]),
                    lifted: true,
                },
            ),
        );
    }

    /// Whether a callable other than the lambdas implemented by `implementations` is lifted along
    /// `sequence`.
    pub(crate) fn lifts_other_than(
        &self,
        sequence: &IrLiftingSequence,
        implementations: &[super::FunId],
    ) -> bool {
        let excluded = implementations
            .iter()
            .filter_map(|implementation| self.lifted_functions.get(implementation))
            .filter(|(lifted, _)| lifted == sequence)
            .filter_map(|(_, site)| Some(site.path.first()?.position))
            .collect::<Vec<_>>();
        self.lifting_sequences
            .get(sequence)
            .is_some_and(|entries| entries.keys().any(|position| !excluded.contains(position)))
    }

    /// Move `implementation`, a source lambda a compiler plugin takes out of the body it is
    /// written in, to `container`, as the only callable lifted along `sequence`. kotlinc's IR
    /// plugins run before its lowering lifts lambdas, so the body it leaves numbers what remains
    /// as though the lambda had never been written there. The lambda declares no callable of its
    /// own.
    pub(crate) fn relocate_lifted_lambda(
        &mut self,
        implementation: super::FunId,
        sequence: IrLiftingSequence,
        source_order: u32,
        container: super::IrEnclosure,
    ) {
        let (written, site) = self
            .lifted_functions
            .remove(&implementation)
            .expect("a relocated lambda was lifted from its source body");
        let [step] = site.path.as_ref() else {
            panic!("a relocated lambda is written directly in a declaration's body");
        };
        if let Some(entries) = self.lifting_sequences.get_mut(&written) {
            entries.remove(&step.position);
        }
        self.place_generated_lambda(sequence, source_order, container, implementation);
    }
}
