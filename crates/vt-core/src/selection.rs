//! Building a [`Selection`] and naming its parts on the command line.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::{Error, bail};

use crate::manifest::{Category, Selection, Tool};

impl Category {
    /// Every category, in display order.
    pub const ALL: [Category; 6] = [
        Category::Sessions,
        Category::Memory,
        Category::Settings,
        Category::Secrets,
        Category::ProjectFiles,
        Category::Cache,
    ];

    /// Command line name (`project-files` for `ProjectFiles`).
    pub fn cli_name(self) -> &'static str {
        match self {
            Category::Sessions => "sessions",
            Category::Memory => "memory",
            Category::Settings => "settings",
            Category::Secrets => "secrets",
            Category::ProjectFiles => "project-files",
            Category::Cache => "cache",
        }
    }

    /// Turkish label for reports and prompts.
    pub fn label(self) -> &'static str {
        match self {
            Category::Sessions => "Oturumlar",
            Category::Memory => "Hafıza",
            Category::Settings => "Ayarlar",
            Category::Secrets => "Sırlar",
            Category::ProjectFiles => "Proje dosyaları",
            Category::Cache => "Önbellek (isteğe bağlı)",
        }
    }
}

impl FromStr for Category {
    type Err = Error;

    /// Accepts the command line name; `_` may stand for `-`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let wanted = s.trim().to_ascii_lowercase().replace('_', "-");
        match Category::ALL.into_iter().find(|c| c.cli_name() == wanted) {
            Some(c) => Ok(c),
            None => bail!(
                "bilinmeyen kategori: {s:?} (geçerli: {})",
                Category::ALL.map(Category::cli_name).join(", ")
            ),
        }
    }
}

impl Tool {
    pub fn cli_name(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
        }
    }
}

impl FromStr for Tool {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(Tool::Claude),
            "codex" => Ok(Tool::Codex),
            _ => bail!("bilinmeyen araç: {s:?} (geçerli: claude, codex)"),
        }
    }
}

impl Selection {
    /// Every category except `Cache`, for each of `tools`.
    pub fn default_for(tools: &[Tool]) -> Self {
        Self::from_flags(tools, &[], &[])
    }

    /// `only` (or, when empty, every category except `Cache`) minus
    /// `exclude`, for each of `tools`. `Cache` is included only when it is
    /// listed in `only`.
    pub fn from_flags(tools: &[Tool], only: &[Category], exclude: &[Category]) -> Self {
        let base: BTreeSet<Category> = if only.is_empty() {
            Category::ALL
                .into_iter()
                .filter(|c| *c != Category::Cache)
                .collect()
        } else {
            only.iter().copied().collect()
        };
        let categories: BTreeSet<Category> =
            base.into_iter().filter(|c| !exclude.contains(c)).collect();
        Self {
            tools: tools.iter().copied().collect(),
            categories: tools
                .iter()
                .map(|t| (*t, categories.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    /// Whether `category` of `tool` is selected.
    pub fn includes(&self, tool: Tool, category: Category) -> bool {
        self.tools.contains(&tool)
            && self
                .categories
                .get(&tool)
                .is_some_and(|c| c.contains(&category))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_excludes_cache_only() {
        let sel = Selection::default_for(&[Tool::Claude]);
        for c in Category::ALL {
            assert_eq!(sel.includes(Tool::Claude, c), c != Category::Cache, "{c:?}");
        }
        assert!(!sel.includes(Tool::Codex, Category::Settings));
    }

    #[test]
    fn flags_only_and_exclude() {
        let sel = Selection::from_flags(&[Tool::Claude], &[], &[Category::Sessions]);
        assert!(!sel.includes(Tool::Claude, Category::Sessions));
        assert!(sel.includes(Tool::Claude, Category::Memory));
        assert!(!sel.includes(Tool::Claude, Category::Cache));

        let sel = Selection::from_flags(
            &[Tool::Claude],
            &[Category::Memory, Category::Cache, Category::Secrets],
            &[Category::Secrets],
        );
        let got: Vec<Category> = sel.categories[&Tool::Claude].iter().copied().collect();
        assert_eq!(got, vec![Category::Memory, Category::Cache]);
    }

    #[test]
    fn parse_names() {
        assert_eq!(
            "project-files".parse::<Category>().unwrap(),
            Category::ProjectFiles
        );
        assert_eq!(
            "Project_Files".parse::<Category>().unwrap(),
            Category::ProjectFiles
        );
        assert!("logs".parse::<Category>().is_err());
        assert_eq!("claude".parse::<Tool>().unwrap(), Tool::Claude);
        assert!("cursor".parse::<Tool>().is_err());
        for c in Category::ALL {
            assert_eq!(c.cli_name().parse::<Category>().unwrap(), c);
        }
    }
}
