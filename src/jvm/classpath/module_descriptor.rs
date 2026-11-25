//! Validation of a JDK module descriptor (`module-info.class`, or `module-info.sig` in `ct.sym`).
//!
//! kotlinc reads every module descriptor of the JDK release it compiles against, with ASM's
//! `ClassReader`, and refuses the compilation when one fails to read. ASM resolves a constant-pool
//! reference by the offset recorded for its index and then by the first `u2` found there, whatever
//! the entry's tag is. A descriptor whose `ModuleMainClass` names a `CONSTANT_Utf8` instead of a
//! `CONSTANT_Class` therefore reads the string's length as a pool index, and fails with an
//! out-of-bounds index. This reader follows the same resolution and reports the same failure.

/// The reason a descriptor does not read, worded as the JVM words the exception ASM raises.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ModuleDescriptorError(String);

impl ModuleDescriptorError {
    fn out_of_bounds(index: usize, length: usize) -> Self {
        Self(format!(
            "java.lang.ArrayIndexOutOfBoundsException: Index {index} out of bounds for length {length}"
        ))
    }

    /// The exception, as `Caused by:` shows it.
    pub(crate) fn cause(&self) -> &str {
        &self.0
    }
}

type Read<T> = Result<T, ModuleDescriptorError>;

struct Descriptor<'a> {
    bytes: &'a [u8],
    /// Offset just past each constant's tag, by pool index; `0` for an index no entry starts at.
    pool: Vec<usize>,
}

impl Descriptor<'_> {
    fn u1(&self, at: usize) -> Read<u8> {
        self.bytes
            .get(at)
            .copied()
            .ok_or_else(|| ModuleDescriptorError::out_of_bounds(at, self.bytes.len()))
    }

    fn u2(&self, at: usize) -> Read<usize> {
        Ok(usize::from(self.u1(at)?) << 8 | usize::from(self.u1(at + 1)?))
    }

    fn u4(&self, at: usize) -> Read<usize> {
        Ok(self.u2(at)? << 16 | self.u2(at + 2)?)
    }

    /// The offset recorded for pool index `index`.
    fn entry(&self, index: usize) -> Read<usize> {
        self.pool
            .get(index)
            .copied()
            .ok_or_else(|| ModuleDescriptorError::out_of_bounds(index, self.pool.len()))
    }

    /// ASM's `readUTF8`: the string a `u2` at `at` indexes; no string for index zero.
    fn utf8(&self, at: usize) -> Read<()> {
        let index = self.u2(at)?;
        if index != 0 {
            let offset = self.entry(index)?;
            // The string's own length field must be readable.
            self.u2(offset)?;
        }
        Ok(())
    }

    /// ASM's `readClass`, `readModule` and `readPackage`: the entry a `u2` at `at` indexes holds a
    /// `u2` that indexes the name.
    fn named(&self, at: usize) -> Read<()> {
        let offset = self.entry(self.u2(at)?)?;
        self.utf8(offset)
    }
}

/// Read the constant pool and the module attributes of `bytes`, the way ASM does.
pub(crate) fn validate(bytes: &[u8]) -> Result<(), ModuleDescriptorError> {
    let mut descriptor = Descriptor {
        bytes,
        pool: Vec::new(),
    };
    let count = descriptor.u2(8)?;
    descriptor.pool = vec![0; count];
    let mut at = 10;
    let mut index = 1;
    while index < count {
        descriptor.pool[index] = at + 1;
        let size = match descriptor.u1(at)? {
            9 | 10 | 11 | 12 | 3 | 4 | 17 | 18 => 5,
            5 | 6 => {
                index += 1;
                9
            }
            1 => 3 + descriptor.u2(at + 1)?,
            15 => 4,
            _ => 3,
        };
        at += size;
        index += 1;
    }
    // access flags, this class, super class
    at += 6;
    at += 2 + 2 * descriptor.u2(at)?;
    // fields and methods
    for _ in 0..2 {
        let members = descriptor.u2(at)?;
        at += 2;
        for _ in 0..members {
            at += 6;
            at = skip_attributes(&descriptor, at)?;
        }
    }
    let attributes = descriptor.u2(at)?;
    at += 2;
    for _ in 0..attributes {
        let name = descriptor.entry(descriptor.u2(at)?)?;
        let length = descriptor.u4(at + 2)?;
        let body = at + 6;
        let text = descriptor
            .u2(name)
            .ok()
            .map(|_| utf8_text(&descriptor, name));
        match text.as_deref() {
            Some("ModuleMainClass") => descriptor.named(body)?,
            Some("ModulePackages") => {
                for package in 0..descriptor.u2(body)? {
                    descriptor.named(body + 2 + 2 * package)?;
                }
            }
            Some("Module") => validate_module(&descriptor, body)?,
            _ => {}
        }
        at = body + length;
    }
    Ok(())
}

fn utf8_text(descriptor: &Descriptor<'_>, offset: usize) -> String {
    let length = descriptor.u2(offset).unwrap_or(0);
    descriptor
        .bytes
        .get(offset + 2..offset + 2 + length)
        .map(|text| String::from_utf8_lossy(text).into_owned())
        .unwrap_or_default()
}

fn skip_attributes(descriptor: &Descriptor<'_>, mut at: usize) -> Read<usize> {
    let attributes = descriptor.u2(at)?;
    at += 2;
    for _ in 0..attributes {
        at += 6 + descriptor.u4(at + 2)?;
    }
    Ok(at)
}

fn validate_module(descriptor: &Descriptor<'_>, mut at: usize) -> Read<()> {
    descriptor.named(at)?;
    descriptor.utf8(at + 4)?;
    at += 6;
    // requires: module, flags, version
    let requires = descriptor.u2(at)?;
    at += 2;
    for _ in 0..requires {
        descriptor.named(at)?;
        descriptor.utf8(at + 4)?;
        at += 6;
    }
    // exports, then opens: package, flags, target modules
    for _ in 0..2 {
        let packages = descriptor.u2(at)?;
        at += 2;
        for _ in 0..packages {
            descriptor.named(at)?;
            let targets = descriptor.u2(at + 4)?;
            at += 6;
            for _ in 0..targets {
                descriptor.named(at)?;
                at += 2;
            }
        }
    }
    let uses = descriptor.u2(at)?;
    at += 2;
    for _ in 0..uses {
        descriptor.named(at)?;
        at += 2;
    }
    let provides = descriptor.u2(at)?;
    at += 2;
    for _ in 0..provides {
        descriptor.named(at)?;
        let implementations = descriptor.u2(at + 2)?;
        at += 4;
        for _ in 0..implementations {
            descriptor.named(at)?;
            at += 2;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor with a pool of `utf8` strings and class entries, and one attribute.
    fn descriptor(pool: &[(u8, &[u8])], attribute: (&str, &[u8])) -> Vec<u8> {
        let mut entries: Vec<(u8, Vec<u8>)> = vec![(1, b"module-info".to_vec())];
        entries.push((7, vec![0, 1]));
        entries.push((1, attribute.0.as_bytes().to_vec()));
        for (tag, body) in pool {
            entries.push((*tag, body.to_vec()));
        }
        let mut out = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 65];
        out.extend(((entries.len() + 1) as u16).to_be_bytes());
        for (tag, body) in &entries {
            out.push(*tag);
            if *tag == 1 {
                out.extend((body.len() as u16).to_be_bytes());
            }
            out.extend(body);
        }
        out.extend([0x80, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        out.extend(3u16.to_be_bytes());
        out.extend((attribute.1.len() as u32).to_be_bytes());
        out.extend(attribute.1);
        out
    }

    #[test]
    fn a_main_class_naming_a_class_entry_reads() {
        // pool: 4 = Utf8 "p/Main", 5 = Class 4
        let bytes = descriptor(
            &[(1, b"p/Main"), (7, &[0, 4])],
            ("ModuleMainClass", &[0, 5]),
        );
        assert_eq!(validate(&bytes), Ok(()));
    }

    #[test]
    fn a_main_class_naming_a_string_reads_its_length_as_an_index() {
        // `p/Main` is 6 bytes long, and the pool holds 5 entries and the unused slot zero.
        let bytes = descriptor(&[(1, b"p/Main")], ("ModuleMainClass", &[0, 4]));
        assert_eq!(
            validate(&bytes).unwrap_err().cause(),
            "java.lang.ArrayIndexOutOfBoundsException: Index 6 out of bounds for length 5"
        );
    }
}
