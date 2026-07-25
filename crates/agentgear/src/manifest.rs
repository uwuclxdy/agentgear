//! serde models for the JSON the crate reads (shipped `plugin.json`, `claude
//! plugin list --json`, `marketplace list --json`) and writes (the generated
//! `marketplace.json`). Every read model is tolerant: unknown fields are ignored
//! (serde default) and optional fields carry `#[serde(default)]`, so an additive
//! CLI-output change does not break parsing.

use serde::{Deserialize, Serialize};

/// A shipped `plugin.json`, read from the embedded tree at materialize time to
/// source the generated marketplace's `description` + `owner`. Name/version are
/// read elsewhere (the derive cross-checks name; build.rs enforces version), so
/// they are not modeled here.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PluginManifest {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<AuthorField>,
}

/// `plugin.json` `author` is either a bare string or an object; `marketplace.json`
/// `owner` is always an object with a name. Normalize both into [`Person`].
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum AuthorField {
    Obj(Person),
    Str(String),
}

impl AuthorField {
    pub fn into_person(self) -> Person {
        match self {
            AuthorField::Obj(p) => p,
            AuthorField::Str(name) => Person { name, email: None, url: None },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Person {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The `marketplace.json` the crate generates. Carries no `version` field: CC
/// keys its cache on the `plugin.json` version, and setting a second version here
/// only masks drift (design §7 rule 1).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MarketplaceManifest {
    pub name: String,
    pub description: String,
    pub owner: Person,
    pub plugins: Vec<MarketplacePlugin>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MarketplacePlugin {
    pub name: String,
    pub source: String,
    pub description: String,
}

/// One entry of `claude plugin list --json`.
#[cfg(feature = "claude")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginEntry {
    /// `"<name>@<marketplace>"`.
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Absolute path to the copied cache tree; a missing dir here means the entry
    /// is registered but its files are gone (a "broken" install).
    #[serde(default)]
    pub install_path: Option<String>,
}

#[cfg(feature = "claude")]
impl PluginEntry {
    pub fn plugin_name(&self) -> &str {
        self.id.split_once('@').map_or(self.id.as_str(), |(n, _)| n)
    }

    pub fn marketplace(&self) -> Option<&str> {
        self.id.split_once('@').map(|(_, m)| m)
    }

    /// True when this entry is `<name>@<marketplace>`.
    pub fn matches(&self, name: &str, marketplace: &str) -> bool {
        self.plugin_name() == name && self.marketplace() == Some(marketplace)
    }
}

/// One entry of `claude plugin marketplace list --json`.
#[cfg(feature = "claude")]
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MarketplaceEntry {
    #[serde(default)]
    pub name: Option<String>,
    /// Source dir for a local-path marketplace; used to detect a dangling
    /// (moved/deleted) marketplace path.
    #[serde(default)]
    pub path: Option<String>,
    /// The pinned git ref of a `source: "github"` entry (`known_marketplaces.json`
    /// stores it first-class; absent for a bare/non-github entry). Lets reconcile
    /// spot a drifted pin and re-point it, since `marketplace update` never moves a
    /// pin (design §ref-pinning ground truth).
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}
