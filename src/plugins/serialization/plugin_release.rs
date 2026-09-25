/// The kotlinx-serialization compiler plugin release krusty reproduces, from the `-Xplugin` jar's
/// manifest (`2.4.10-release-377`). The plugin ships inside kotlinc and is released with it, so this
/// is the kotlinc release the jar came from; its checkers' wording follows it, independent of the
/// Kotlin version krusty targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PluginRelease {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl PluginRelease {
    /// kotlinc 2.4.20's plugin ends its checker messages with a full stop.
    pub const V2_4_20: PluginRelease = PluginRelease::new(2, 4, 20);

    pub const fn new(major: u32, minor: u32, patch: u32) -> PluginRelease {
        PluginRelease {
            major,
            minor,
            patch,
        }
    }

    /// Parse a release by its leading `major.minor.patch`, ignoring a build suffix
    /// (`2.4.10-release-377`, `2.4.20`). `None` when the leading components are not numeric.
    pub fn parse(release: &str) -> Option<PluginRelease> {
        let numbers = release.split(['-', '+']).next()?;
        let mut parts = numbers.split('.').map(str::parse::<u32>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = parts.next().unwrap_or(Ok(0)).ok()?;
        Some(PluginRelease::new(major, minor, patch))
    }
}

#[cfg(test)]
mod tests {
    use super::PluginRelease;

    #[test]
    fn parses_the_leading_version() {
        assert_eq!(
            PluginRelease::parse("2.4.10-release-377"),
            Some(PluginRelease::new(2, 4, 10))
        );
        assert_eq!(
            PluginRelease::parse("2.4.20"),
            Some(PluginRelease::new(2, 4, 20))
        );
        assert_eq!(
            PluginRelease::parse("2.5"),
            Some(PluginRelease::new(2, 5, 0))
        );
        assert_eq!(PluginRelease::parse("dev"), None);
        assert!(PluginRelease::new(2, 4, 10) < PluginRelease::V2_4_20);
    }
}
