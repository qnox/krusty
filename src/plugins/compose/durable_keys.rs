//! The durable group keys kotlinc's `DurableFunctionKeyTransformer` derives from a declaration's
//! place in its file, and the package hash its source information names a file by.

/// Java's `String.hashCode` over `text`'s UTF-16 code units.
pub(super) fn java_string_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

/// Kotlin's `Int.toString(radix)`: lowercase digits, a leading `-` for a negative value.
pub(super) fn radix_string(value: i32, radix: u32) -> String {
    let mut magnitude = i64::from(value).unsigned_abs();
    if magnitude == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    while magnitude > 0 {
        let digit = (magnitude % u64::from(radix)) as u32;
        digits.push(char::from_digit(digit, radix).expect("a digit below the radix"));
        magnitude /= u64::from(radix);
    }
    if value < 0 {
        digits.push('-');
    }
    digits.into_iter().rev().collect()
}

/// `IrFunction.packageHash()`: the package name folded as `hash * 31 + char`, made non-negative
/// with Kotlin's `absoluteValue` (which leaves `Int.MIN_VALUE` as it is); `0` for the root package.
pub(super) fn package_hash(package: &str) -> i32 {
    package
        .chars()
        .fold(0i32, |hash, character| {
            hash.wrapping_mul(31).wrapping_add(character as i32)
        })
        .wrapping_abs()
}

/// One declaration's place on the durable key path, innermost first, as `DurableKeyVisitor`
/// prints it: `fun-<signature>` under the package and file scopes.
pub(super) struct KeyPath<'a> {
    /// `IrSimpleFunction.signatureString()`: `[Receiver.]name(Param,…)Result`, each type by its
    /// classifier's simple name.
    pub signature: &'a str,
    /// The file's package as a dotted name; empty for the root package.
    pub package: &'a str,
    /// The file's simple name (`a.kt`).
    pub file_name: &'a str,
}

impl KeyPath<'_> {
    /// The key string the transformer hashes.
    pub(super) fn path(&self) -> String {
        format!(
            "fun-{}/pkg-{}/file-{}",
            self.signature, self.package, self.file_name
        )
    }

    /// The group key: the path's Java hash.
    pub(super) fn key(&self) -> i32 {
        java_string_hash(&self.path())
    }

    /// The key of a composable lambda passed as argument `argument` of a call of `callee` in this
    /// declaration's body: its path enters the call, then the argument, then the lambda. `sibling`
    /// numbers the calls of `callee` the body made before this one, which `DurableKeyVisitor` keys
    /// apart as `call-<callee>:<n>`.
    pub(super) fn lambda_key(&self, callee: &str, sibling: u32, argument: u32) -> i32 {
        let call = match sibling {
            0 => format!("call-{callee}"),
            n => format!("call-{callee}:{n}"),
        };
        java_string_hash(&format!(
            "fun-$anonymous$()Unit/arg-{argument}/{call}/{}",
            self.path()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys kotlinc 2.4.21's Compose plugin emitted for these declarations.
    #[test]
    fn function_keys_match_kotlinc() {
        let key = |signature, package, file_name| {
            KeyPath {
                signature,
                package,
                file_name,
            }
            .key()
        };
        assert_eq!(key("Greeting(String)Unit", "demo", "a.kt"), 1219023440);
        assert_eq!(key("A()Unit", "demo", "a.kt"), 2014676179);
        assert_eq!(key("B()Unit", "demo", "a.kt"), 2045695986);
    }

    /// Keys kotlinc 2.4.21's Compose plugin gave the two lambdas `Card(title: String)` in package
    /// `probe` passes to its first and second call of `Box`.
    #[test]
    fn lambda_keys_match_kotlinc() {
        let card = KeyPath {
            signature: "Card(String)Unit",
            package: "probe",
            file_name: "Probe.kt",
        };
        assert_eq!(card.lambda_key("Box", 0, 0), -300906309);
        assert_eq!(card.lambda_key("Box", 1, 0), -1215453532);
    }

    #[test]
    fn package_hash_names_the_file_as_kotlinc_does() {
        assert_eq!(radix_string(package_hash("demo"), 36), "1u09v");
        assert_eq!(package_hash(""), 0);
    }

    #[test]
    fn radix_string_spells_negative_values_with_a_sign() {
        assert_eq!(radix_string(-35, 36), "-z");
        assert_eq!(radix_string(i32::MIN, 36), "-zik0zk");
    }
}
