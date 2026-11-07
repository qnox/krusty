//! Checked annotation argument membership and written order, independent of runtime default filling.

/// Declaration parameter ordinals survive closed-default folding and replay. A supplied but
/// unwritten empty vararg differs from an omitted declaration default, even when both have values
/// at runtime. `None` records unknown written order; `Some([])` records no written arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationArgumentProvenance {
    parameter_count: u32,
    written: Option<Box<[u32]>>,
    omitted: Box<[u32]>,
}

impl AnnotationArgumentProvenance {
    pub(crate) fn checked(
        parameter_count: usize,
        written: Option<Vec<u32>>,
        omitted: Vec<u32>,
    ) -> Option<Self> {
        let parameter_count = u32::try_from(parameter_count).ok()?;
        let valid = |ordinals: &[u32]| {
            ordinals.iter().enumerate().all(|(index, ordinal)| {
                *ordinal < parameter_count && !ordinals[..index].contains(ordinal)
            })
        };
        if !valid(&omitted)
            || written.as_ref().is_some_and(|written| {
                !valid(written) || written.iter().any(|ordinal| omitted.contains(ordinal))
            })
        {
            return None;
        }
        Some(Self {
            parameter_count,
            written: written.map(Vec::into_boxed_slice),
            omitted: omitted.into_boxed_slice(),
        })
    }

    /// Selected source slots precede implicit empty-vararg/default synthesis. Repeated vararg
    /// contributions write one member, at the first occurrence of its declaration ordinal.
    pub(crate) fn from_selected(
        parameter_count: usize,
        slots: &[usize],
        omitted: &[usize],
    ) -> Option<Self> {
        let mut written = Vec::new();
        for slot in slots {
            let ordinal = u32::try_from(*slot).ok()?;
            if !written.contains(&ordinal) {
                written.push(ordinal);
            }
        }
        let omitted = omitted
            .iter()
            .map(|ordinal| u32::try_from(*ordinal).ok())
            .collect::<Option<_>>()?;
        Self::checked(parameter_count, Some(written), omitted)
    }

    pub(crate) fn omits(&self, parameter: u32) -> bool {
        self.omitted.contains(&parameter)
    }

    /// Project declaration ordinals into the compact, declaration-ordered emitted value list.
    /// The outer option rejects an inconsistent shape or a metadata index overflow.
    pub(crate) fn applied_order(&self, parameter_count: usize) -> Option<Option<Box<[u16]>>> {
        if usize::try_from(self.parameter_count).ok()? != parameter_count {
            return None;
        }
        Some(match &self.written {
            None => None,
            Some(written) => Some(
                written
                    .iter()
                    .map(|ordinal| {
                        let before = self
                            .omitted
                            .iter()
                            .filter(|omitted| *omitted < ordinal)
                            .count();
                        u16::try_from(usize::try_from(*ordinal).ok()?.checked_sub(before)?).ok()
                    })
                    .collect::<Option<_>>()?,
            ),
        })
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        (self.written.as_ref().map_or(0, |written| written.len()) + self.omitted.len())
            * std::mem::size_of::<u32>()
    }
}

#[cfg(test)]
mod tests {
    use super::AnnotationArgumentProvenance as Provenance;

    #[test]
    fn written_members_project_past_omissions_and_keep_unwritten_varargs() {
        let provenance = Provenance::from_selected(4, &[2, 1, 3, 3], &[0]).unwrap();
        assert_eq!(
            provenance.applied_order(4),
            Some(Some(vec![1, 0, 2].into_boxed_slice()))
        );
        assert_eq!(provenance.applied_order(3), None);
        let unwritten = Provenance::from_selected(4, &[2, 1], &[0]).unwrap();
        assert_eq!(
            unwritten.applied_order(4),
            Some(Some(vec![1, 0].into_boxed_slice()))
        );
    }

    #[test]
    fn unknown_order_differs_from_an_authentic_empty_written_list() {
        let unknown = Provenance::checked(1, None, vec![0]).unwrap();
        let unwritten = Provenance::from_selected(1, &[], &[0]).unwrap();
        assert_eq!(unknown.applied_order(1), Some(None));
        assert_eq!(
            unwritten.applied_order(1),
            Some(Some(Vec::new().into_boxed_slice()))
        );
        assert_ne!(unknown, unwritten);
    }

    #[test]
    fn inconsistent_declaration_ordinals_are_rejected() {
        for (written, omitted) in [
            (Some(vec![1]), vec![]),
            (None, vec![1]),
            (Some(vec![0, 0]), vec![]),
            (None, vec![0, 0]),
            (Some(vec![0]), vec![0]),
        ] {
            assert_eq!(Provenance::checked(1, written, omitted), None);
        }
        let overflow = Provenance::checked(65_537, Some(vec![65_536]), vec![]).unwrap();
        assert_eq!(overflow.applied_order(65_537), None);
    }
}
