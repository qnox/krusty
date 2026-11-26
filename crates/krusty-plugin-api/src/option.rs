/// One `-P plugin:<id>:<key>=<value>` option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOption {
    pub id: String,
    pub key: String,
    pub value: String,
}
