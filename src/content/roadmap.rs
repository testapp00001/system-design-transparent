//! The roadmap is a map of *everything* a backend engineer may run into,
//! including topics nobody has written about yet. Seeing that a thing exists
//! is the first step to learning it.

use std::{fs, path::Path};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roadmap {
    #[serde(rename = "section", default)]
    pub sections: Vec<Section>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "topic", default)]
    pub topics: Vec<Topic>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topic {
    pub title: String,
    /// Slug of the article covering this topic, if one exists yet.
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default)]
    pub note: String,
}

impl Roadmap {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let src = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&src).with_context(|| format!("parsing {}", path.display()))
    }
}
