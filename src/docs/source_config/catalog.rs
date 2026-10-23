use std::collections::BTreeMap;

use crate::{Config, SourceGraph, SourceType};

#[derive(Clone, Debug)]
pub(crate) struct SourceEntry {
    pub(crate) name: String,
    pub(crate) source_type: SourceType,
    pub(crate) members: Vec<String>,
    pub(crate) cli_port: Option<u16>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SourceCatalog {
    ecosystems: BTreeMap<&'static str, Vec<SourceEntry>>,
}

impl SourceCatalog {
    pub(crate) fn from_config(config: &Config) -> Self {
        let mut catalog = Self::default();
        catalog.add("rustup", &config.rustup.sources, None);
        if let Some(cargo) = config.cargo.as_ref() {
            catalog.add("cargo", &cargo.sources, None);
        }
        if let Some(npm) = config.npm.as_ref() {
            catalog.add("npm", &npm.sources, None);
        }
        if let Some(nuget) = config.nuget.as_ref() {
            catalog.add("nuget", &nuget.sources, None);
        }
        if let Some(upm) = config.upm.as_ref() {
            catalog.add("upm", &upm.sources, None);
        }
        if let Some(vscode) = config.vscode.as_ref() {
            catalog.add("vscode", &vscode.sources, None);
        }
        if let Some(docker) = config.docker.as_ref() {
            catalog.add("docker", &docker.sources, Some(&docker.cli_listen_ports));
        }
        catalog
    }

    fn add(
        &mut self,
        ecosystem: &'static str,
        graph: &SourceGraph,
        ports: Option<&BTreeMap<String, u16>>,
    ) {
        self.ecosystems.insert(
            ecosystem,
            graph
                .entries()
                .map(|source| SourceEntry {
                    name: source.name.clone(),
                    source_type: source.source_type,
                    members: source.members.clone(),
                    cli_port: ports.and_then(|ports| ports.get(&source.name).copied()),
                })
                .collect(),
        );
    }

    pub(crate) fn sources(&self, ecosystem: &str) -> &[SourceEntry] {
        self.ecosystems
            .get(ecosystem)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn recommended(&self, ecosystem: &str) -> Option<&SourceEntry> {
        let sources = self.sources(ecosystem);
        sources
            .iter()
            .find(|source| source.source_type == SourceType::Group)
            .or_else(|| sources.first())
    }

    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        let entry = |name: &str, source_type: SourceType, members: &[&str], cli_port| SourceEntry {
            name: name.to_owned(),
            source_type,
            members: members.iter().map(|member| (*member).to_owned()).collect(),
            cli_port,
        };
        let mut catalog = Self::default();
        catalog.ecosystems.insert(
            "rustup",
            vec![
                entry("official", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["official"], None),
            ],
        );
        catalog.ecosystems.insert(
            "cargo",
            vec![
                entry("official", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["official"], None),
            ],
        );
        catalog.ecosystems.insert(
            "npm",
            vec![
                entry("npmjs", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["npmjs"], None),
            ],
        );
        catalog.ecosystems.insert(
            "nuget",
            vec![
                entry("official", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["official"], None),
            ],
        );
        catalog.ecosystems.insert(
            "upm",
            vec![
                entry("unity", SourceType::Proxy, &[], None),
                entry("openupm", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["unity", "openupm"], None),
            ],
        );
        catalog.ecosystems.insert(
            "vscode",
            vec![
                entry("official", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["official"], None),
            ],
        );
        catalog.ecosystems.insert(
            "docker",
            vec![
                entry("official", SourceType::Proxy, &[], None),
                entry("group", SourceType::Group, &["official"], Some(19000)),
            ],
        );
        catalog
    }
}
