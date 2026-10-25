//! The interface tables of the program's types, numbered and written by the link (see
//! `native::interface_calls`).
//!
//! Only a member whose stub looks a table up needs an entry in any table. Those members are
//! numbered here, program-wide, each with the lowest index no other such member already holds in
//! a table of any type implementing it: two members no type implements both share an index. Each
//! type's table then ends after the highest index it implements, and its `KType.itable` field is
//! pointed at it.

use crate::fx_hash::{HashMap, HashSet};

use super::super::interface_calls::ITABLE_OFFSET;
use super::interface_records::Target;
use super::ProgramLinkError;

/// One type's table: where it starts among the tables, and the function at each index, `None`
/// where the type implements no member numbered there.
#[derive(Debug)]
struct Table {
    ty: Target,
    offset: u64,
    entries: Vec<Option<Target>>,
}

/// Every table of the program, and each table-reading member's index.
#[derive(Debug, Default)]
pub(super) struct InterfaceTables {
    tables: Vec<Table>,
    size: u64,
}

impl InterfaceTables {
    /// Number the members `members` lists, each by its implementations `(type, function)`, and
    /// lay out the tables they need. Returns the tables and each member's index, parallel to
    /// `members`. The members are numbered in the order given, which the caller keeps
    /// deterministic.
    pub(super) fn number<'r>(
        members: impl IntoIterator<Item = &'r [(Target, Target)]>,
    ) -> Result<(Self, Vec<u32>), ProgramLinkError> {
        let mut taken: HashMap<&Target, HashSet<u32>> = HashMap::default();
        let mut tables: Vec<Table> = Vec::new();
        let mut table_of: HashMap<&Target, usize> = HashMap::default();
        let mut indices = Vec::new();
        for implementations in members {
            let types: Vec<&Target> = implementations.iter().map(|(ty, _)| ty).collect();
            let index = (0..)
                .find(|index| {
                    types
                        .iter()
                        .all(|ty| taken.get(ty).is_none_or(|used| !used.contains(index)))
                })
                .expect("an unbounded range has a free index");
            for (ty, function) in implementations {
                taken.entry(ty).or_default().insert(index);
                let table = *table_of.entry(ty).or_insert_with(|| {
                    tables.push(Table {
                        ty: ty.clone(),
                        offset: 0,
                        entries: Vec::new(),
                    });
                    tables.len() - 1
                });
                let entries = &mut tables[table].entries;
                if entries.len() <= index as usize {
                    entries.resize(index as usize + 1, None);
                }
                match &entries[index as usize] {
                    Some(known) if known != function => {
                        return Err(ProgramLinkError::Unsupported(
                            "a type records two implementations of one interface member".into(),
                        ))
                    }
                    _ => entries[index as usize] = Some(function.clone()),
                }
            }
            indices.push(index);
        }
        let mut size = 0;
        for table in &mut tables {
            table.offset = size;
            size += table.entries.len() as u64 * 8;
        }
        Ok((Self { tables, size }, indices))
    }

    /// The bytes the tables take, from a word boundary.
    pub(super) fn size(&self) -> u64 {
        self.size
    }

    /// Write every table into `image`, the tables starting at file offset `offset` and address
    /// `at`, and point each type's `itable` field at its table. `address` resolves what a record
    /// names; `file_offset` finds the image bytes of an address the link placed.
    pub(super) fn write(
        &self,
        image: &mut [u8],
        offset: u64,
        at: u64,
        address: impl Fn(&Target) -> Result<u64, ProgramLinkError>,
        file_offset: impl Fn(u64) -> Option<u64>,
    ) -> Result<(), ProgramLinkError> {
        for table in &self.tables {
            for (index, entry) in table.entries.iter().enumerate() {
                let value = match entry {
                    Some(function) => address(function)?,
                    None => 0,
                };
                let start = (offset + table.offset) as usize + index * 8;
                image[start..start + 8].copy_from_slice(&value.to_le_bytes());
            }
            let field = address(&table.ty)? + u64::from(ITABLE_OFFSET);
            let start = file_offset(field)
                .and_then(|start| usize::try_from(start).ok())
                .filter(|start| start + 8 <= image.len())
                .ok_or_else(|| {
                    ProgramLinkError::Unsupported(
                        "a type with an interface table has no bytes in the image".into(),
                    )
                })?;
            if image[start..start + 8] != [0; 8] {
                return Err(ProgramLinkError::Unsupported(
                    "a type's interface table field is already filled".into(),
                ));
            }
            let table_at = at + table.offset;
            image[start..start + 8].copy_from_slice(&table_at.to_le_bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> Target {
        Target::Global(name.into())
    }

    fn pairs(pairs: &[(&str, &str)]) -> Vec<(Target, Target)> {
        pairs
            .iter()
            .map(|(ty, function)| (target(ty), target(function)))
            .collect()
    }

    #[test]
    fn members_no_type_implements_both_share_an_index() {
        let first = pairs(&[("A", "a1"), ("B", "b1"), ("C", "c1")]);
        let second = pairs(&[("D", "d1"), ("E", "e1"), ("F", "f1")]);
        let (tables, indices) =
            InterfaceTables::number([first.as_slice(), second.as_slice()]).unwrap();
        assert_eq!(indices, [0, 0]);
        assert_eq!(tables.size(), 6 * 8);
    }

    #[test]
    fn members_one_type_implements_take_distinct_indices() {
        let first = pairs(&[("A", "a1"), ("B", "b1"), ("C", "c1")]);
        let second = pairs(&[("A", "a2"), ("D", "d2"), ("E", "e2")]);
        let third = pairs(&[("D", "d3"), ("F", "f3"), ("G", "g3")]);
        let (tables, indices) =
            InterfaceTables::number([first.as_slice(), second.as_slice(), third.as_slice()])
                .unwrap();
        // The second member meets A's 0 and takes 1; the third meets only D's 1 and takes 0.
        assert_eq!(indices, [0, 1, 0]);
        // A: 2 entries, B and C: 1, D: 2, E: 2 (its 0 empty), F and G: 1.
        assert_eq!(tables.size(), (2 + 1 + 1 + 2 + 2 + 1 + 1) * 8);
    }

    #[test]
    fn two_implementations_for_one_type_are_refused() {
        let member = pairs(&[("A", "a1"), ("A", "a2"), ("B", "b1")]);
        assert_eq!(
            InterfaceTables::number([member.as_slice()]).unwrap_err(),
            ProgramLinkError::Unsupported(
                "a type records two implementations of one interface member".into()
            )
        );
    }
}
