//! Module descriptors of a JDK jimage: which packages each named module exports to everyone.
//!
//! Code outside every named module (a compilation with no `module-info`) reads a module's packages
//! only through its unqualified `exports`. A qualified `exports p to m` does not make `p` reachable
//! to it. The descriptor is `/<module>/module-info.class`; its `Module` attribute carries the
//! export table.

use std::sync::OnceLock;

use crate::fx_hash::HashSet;
use crate::name_tree::{NameId, NameTree};

/// One named module of the image: its name, the location of its descriptor resource, and the
/// export table parsed from that descriptor on first use.
#[derive(Debug)]
pub(super) struct JimageModule {
    name: Box<str>,
    descriptor: Option<(u64, usize, bool)>,
    exports: OnceLock<Option<HashSet<NameId>>>,
}

impl JimageModule {
    pub(super) fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            descriptor: None,
            exports: OnceLock::new(),
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn set_descriptor(&mut self, offset: u64, size: usize, compressed: bool) {
        self.descriptor = Some((offset, size, compressed));
    }

    /// The packages this module exports without qualification, as identities in `names`. `None`
    /// when the image holds no readable descriptor for the module: nothing is then known to be
    /// hidden.
    pub(super) fn exports(
        &self,
        names: &NameTree,
        read: impl FnOnce((u64, usize, bool)) -> Option<Vec<u8>>,
    ) -> Option<&HashSet<NameId>> {
        self.exports
            .get_or_init(|| {
                let bytes = read(self.descriptor?)?;
                let packages = unqualified_exports(&bytes)?;
                Some(
                    packages
                        .iter()
                        .filter_map(|package| names.get(package))
                        .collect(),
                )
            })
            .as_ref()
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u1(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u2(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u4(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
}

enum Constant<'a> {
    Utf8(&'a str),
    Package(u16),
    Other,
}

/// The internal names (`a/b`) of the packages a `module-info.class` exports without a target list.
fn unqualified_exports(bytes: &[u8]) -> Option<Vec<String>> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.u4()? != 0xCAFE_BABE {
        return None;
    }
    reader.take(4)?; // minor and major version
    let count = reader.u2()? as usize;
    let mut constants = Vec::with_capacity(count);
    constants.push(Constant::Other);
    while constants.len() < count {
        let tag = reader.u1()?;
        let (constant, slots) = match tag {
            1 => {
                let length = reader.u2()? as usize;
                // Descriptor strings are modified UTF-8; package names are ASCII-compatible, and
                // an entry that is not valid UTF-8 is never one a package lookup can match.
                let text = std::str::from_utf8(reader.take(length)?).unwrap_or("");
                (Constant::Utf8(text), 1)
            }
            3 | 4 | 9..=12 | 17 | 18 => {
                reader.take(4)?;
                (Constant::Other, 1)
            }
            5 | 6 => {
                reader.take(8)?;
                (Constant::Other, 2)
            }
            7 | 8 | 16 | 19 => {
                reader.take(2)?;
                (Constant::Other, 1)
            }
            15 => {
                reader.take(3)?;
                (Constant::Other, 1)
            }
            20 => (Constant::Package(reader.u2()?), 1),
            _ => return None,
        };
        constants.push(constant);
        if slots == 2 {
            constants.push(Constant::Other);
        }
    }
    let utf8 = |index: u16| match constants.get(index as usize)? {
        Constant::Utf8(text) => Some(*text),
        _ => None,
    };
    reader.take(6)?; // access flags, this class, super class
    let interfaces = reader.u2()? as usize;
    reader.take(interfaces * 2)?;
    // A module descriptor declares no fields or methods.
    if reader.u2()? != 0 || reader.u2()? != 0 {
        return None;
    }
    for _ in 0..reader.u2()? {
        let name = utf8(reader.u2()?)?;
        let length = reader.u4()? as usize;
        let body = reader.take(length)?;
        if name != "Module" {
            continue;
        }
        let mut attribute = Reader { bytes: body, at: 0 };
        attribute.take(6)?; // module name, flags, version
        let requires = attribute.u2()? as usize;
        attribute.take(requires * 6)?;
        let mut exported = Vec::new();
        for _ in 0..attribute.u2()? {
            let package = match constants.get(attribute.u2()? as usize)? {
                Constant::Package(name) => utf8(*name)?,
                _ => return None,
            };
            attribute.take(2)?; // export flags
            let targets = attribute.u2()? as usize;
            attribute.take(targets * 2)?;
            if targets == 0 {
                exported.push(package.to_string());
            }
        }
        return Some(exported);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf8(out: &mut Vec<u8>, text: &str) {
        out.push(1);
        out.extend((text.len() as u16).to_be_bytes());
        out.extend(text.as_bytes());
    }

    /// A descriptor exporting `open/api` to everyone and `hidden/impl` only to `friend`.
    fn descriptor() -> Vec<u8> {
        let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 53];
        out.extend(13u16.to_be_bytes());
        utf8(&mut out, "Module"); // 1
        utf8(&mut out, "m"); // 2
        out.extend([19, 0, 2]); // 3: Module m
        utf8(&mut out, "open/api"); // 4
        out.extend([20, 0, 4]); // 5: Package open/api
        utf8(&mut out, "hidden/impl"); // 6
        out.extend([20, 0, 6]); // 7: Package hidden/impl
        utf8(&mut out, "friend"); // 8
        out.extend([19, 0, 8]); // 9: Module friend
        out.extend([5, 0, 0, 0, 0, 0, 0, 0, 1]); // 10..11: Long, two slots
        utf8(&mut out, "module-info"); // 12
        out.extend([0x80, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        out.extend(1u16.to_be_bytes()); // attribute name: Module
        let mut module = Vec::new();
        module.extend([0, 3, 0, 0, 0, 0]); // name, flags, version
        module.extend([0, 0]); // requires
        module.extend(2u16.to_be_bytes());
        module.extend([0, 5, 0, 0, 0, 0]); // open/api, no targets
        module.extend([0, 7, 0, 0, 0, 1, 0, 9]); // hidden/impl to friend
        out.extend((module.len() as u32).to_be_bytes());
        out.extend(module);
        out
    }

    #[test]
    fn only_unqualified_exports_are_reachable_without_a_module() {
        assert_eq!(
            unqualified_exports(&descriptor()),
            Some(vec!["open/api".to_string()])
        );
    }

    #[test]
    fn a_truncated_or_foreign_descriptor_names_nothing() {
        let bytes = descriptor();
        assert_eq!(unqualified_exports(&bytes[..bytes.len() - 3]), None);
        assert_eq!(unqualified_exports(b"not a class file"), None);
    }
}
