//! Physical parameter shapes read from already-selected JVM descriptors.

/// Each parameter descriptor in `(…)ret`, including array depth and object terminators.
pub(super) fn types(descriptor: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return out;
    };
    let mut index = 1;
    while index < end {
        let start = index;
        while index < end && bytes[index] == b'[' {
            index += 1;
        }
        if index < end && bytes[index] == b'L' {
            while index < end && bytes[index] != b';' {
                index += 1;
            }
        }
        index += 1;
        out.push(descriptor[start..index].to_string());
    }
    out
}

/// Whether each descriptor parameter occupies a JVM reference slot.
pub(super) fn references(descriptor: &str) -> Vec<bool> {
    let mut out = Vec::new();
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return out;
    };
    let mut index = 1;
    while index < end {
        match bytes[index] {
            b'[' => {
                out.push(true);
                index += 1;
                while index < end && bytes[index] == b'[' {
                    index += 1;
                }
                if index < end && bytes[index] == b'L' {
                    while index < end && bytes[index] != b';' {
                        index += 1;
                    }
                }
                index += 1;
            }
            b'L' => {
                out.push(true);
                while index < end && bytes[index] != b';' {
                    index += 1;
                }
                index += 1;
            }
            _ => {
                out.push(false);
                index += 1;
            }
        }
    }
    out
}
