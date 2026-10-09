//! Reading a private repository's committed manifests and term declaration.
//!
//! Everything is read out of the commit the checkout's `HEAD` names, through git's
//! object store: a file somebody is editing, has not added, or has ignored is never
//! read, so what a check derives is what the repository has committed and nothing a
//! dirty worktree happens to say.
//!
//! Supported, and nothing else:
//!
//! - `Cargo.toml`: `[package].name`, and every `[workspace].members` entry's own
//!   `Cargo.toml` `[package].name`;
//! - `package.json`: `name`, and every `workspaces` entry's (an array, or an object's
//!   `packages` array) own `package.json` `name` — a scoped `@scope/name` also yields
//!   `name`;
//! - `pyproject.toml`: `[project].name` and `[tool.poetry].name`.
//!
//! A member entry may use `*` within a path segment. A member named literally whose
//! manifest is not committed, and any manifest that does not parse, is refused: a
//! required declaration that cannot be read never reads as one that declares nothing.

use std::collections::BTreeSet;
use std::path::Path;

use super::{PrivateTerms, TermSource, PRIVATE_TERMS_FILE, PRIVATE_TERMS_VERSION};

/// A failure, in words that name a manifest and never a term.
pub type Failure = String;

/// The term source a registered checkout's committed tree describes.
pub fn committed_source(checkout: &Path, identity: &str) -> Result<TermSource, Failure> {
    let (owner, name) = parts(identity);
    let repository = git2::Repository::open(checkout)
        .map_err(|error| format!("the checkout is not a readable repository: {error}"))?;
    let tree = match repository.head() {
        Ok(head) => Some(
            head.peel_to_tree()
                .map_err(|error| format!("HEAD names no readable tree: {error}"))?,
        ),
        Err(error)
            if error.code() == git2::ErrorCode::UnbornBranch
                || error.code() == git2::ErrorCode::NotFound =>
        {
            None
        }
        Err(error) => return Err(format!("HEAD cannot be read: {error}")),
    };
    let mut packages = BTreeSet::new();
    let mut declaration = None;
    if let Some(tree) = &tree {
        let committed = Committed {
            repository: &repository,
            tree,
        };
        cargo(&committed, &mut packages)?;
        npm(&committed, &mut packages)?;
        python(&committed, &mut packages)?;
        if let Some(text) = committed.text(PRIVATE_TERMS_FILE)? {
            declaration = Some(parse_declaration(&text)?);
        }
    }
    Ok(TermSource {
        identity: identity.to_owned(),
        owner,
        name,
        packages,
        declaration,
    })
}

/// The owner and bare name an identity key spells: `host/owner/name`, or a local
/// path's last segment as a name with no owner.
fn parts(identity: &str) -> (Option<String>, String) {
    let normalized = crate::store::normalize(identity);
    match normalized.hosted {
        Some(hosted) => (Some(hosted.owner), hosted.name),
        None => {
            let name = Path::new(&normalized.key)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (None, name)
        }
    }
}

/// A declaration's text, held to its schema.
pub fn parse_declaration(text: &str) -> Result<PrivateTerms, Failure> {
    let declaration: PrivateTerms = toml::from_str(text)
        .map_err(|error| format!("{PRIVATE_TERMS_FILE} does not parse: {}", error.message()))?;
    if declaration.schema_version != PRIVATE_TERMS_VERSION {
        return Err(format!(
            "{PRIVATE_TERMS_FILE} declares schema_version {}; this build reads \
             {PRIVATE_TERMS_VERSION}",
            declaration.schema_version
        ));
    }
    Ok(declaration)
}

/// One commit's tree, read file by file.
struct Committed<'r> {
    repository: &'r git2::Repository,
    tree: &'r git2::Tree<'r>,
}

impl Committed<'_> {
    /// The committed text at `path`, or `None` where nothing is committed there.
    fn text(&self, path: &str) -> Result<Option<String>, Failure> {
        let entry = match self.tree.get_path(Path::new(path)) {
            Ok(entry) => entry,
            Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(format!("{path} cannot be read: {error}")),
        };
        let blob = entry
            .to_object(self.repository)
            .and_then(|object| object.peel_to_blob())
            .map_err(|error| format!("{path} is not a readable file: {error}"))?;
        String::from_utf8(blob.content().to_vec())
            .map(Some)
            .map_err(|_| format!("{path} is not UTF-8"))
    }

    /// Every committed directory a member pattern names, as paths from the root.
    fn expand(&self, pattern: &str) -> Result<Vec<String>, Failure> {
        let segments: Vec<&str> = pattern
            .trim_start_matches("./")
            .trim_end_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect();
        if segments.is_empty() || segments.contains(&"..") {
            return Err(format!(
                "member pattern {pattern:?} is not inside the repository"
            ));
        }
        let mut found = vec![(String::new(), self.tree.clone())];
        for segment in segments {
            let mut next = Vec::new();
            for (prefix, tree) in &found {
                for entry in tree.iter() {
                    let Ok(name) = entry.name() else { continue };
                    if entry.kind() != Some(git2::ObjectType::Tree)
                        || !crate::policy::glob(segment, name)
                    {
                        continue;
                    }
                    let subtree = entry
                        .to_object(self.repository)
                        .and_then(|object| object.peel_to_tree())
                        .map_err(|error| format!("{name} cannot be read: {error}"))?;
                    let path = if prefix.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    next.push((path, subtree));
                }
            }
            found = next;
        }
        Ok(found.into_iter().map(|(path, _)| path).collect())
    }

    /// Each member's manifest text: a literal member must have one, and a glob takes
    /// what it finds. A directory `excluded` names is no member.
    fn members(
        &self,
        patterns: &[String],
        excluded: &[String],
        manifest: &str,
    ) -> Result<Vec<String>, Failure> {
        let excluded: Vec<&str> = excluded
            .iter()
            .map(|path| path.trim_start_matches("./").trim_end_matches('/'))
            .collect();
        let mut texts = Vec::new();
        for pattern in patterns {
            let literal = !pattern.contains('*');
            let mut directories = self.expand(pattern)?;
            directories.retain(|directory| !excluded.contains(&directory.as_str()));
            if literal && excluded.contains(&pattern.trim_start_matches("./").trim_end_matches('/'))
            {
                continue;
            }
            if literal && directories.is_empty() {
                return Err(format!("member {pattern:?} is not committed"));
            }
            for directory in directories {
                let path = format!("{directory}/{manifest}");
                match self.text(&path)? {
                    Some(text) => texts.push(text),
                    None if literal => return Err(format!("{path} is not committed")),
                    None => {}
                }
            }
        }
        Ok(texts)
    }
}

fn toml_table(path: &str, text: &str) -> Result<toml::Table, Failure> {
    text.parse::<toml::Table>()
        .map_err(|error| format!("{path} does not parse: {}", error.message()))
}

fn string_at<'t>(table: &'t toml::Table, path: &[&str]) -> Option<&'t str> {
    let (last, parents) = path.split_last()?;
    let mut node = table;
    for key in parents {
        node = node.get(*key)?.as_table()?;
    }
    node.get(*last)?.as_str()
}

fn strings(value: Option<&toml::Value>, what: &str) -> Result<Vec<String>, Failure> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{what} is not an array"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{what} holds something that is not a string"))
        })
        .collect()
}

fn cargo(committed: &Committed<'_>, packages: &mut BTreeSet<String>) -> Result<(), Failure> {
    let Some(text) = committed.text("Cargo.toml")? else {
        return Ok(());
    };
    let root = toml_table("Cargo.toml", &text)?;
    if let Some(name) = string_at(&root, &["package", "name"]) {
        packages.insert(name.to_owned());
    }
    let workspace = root.get("workspace").and_then(toml::Value::as_table);
    let members = strings(
        workspace.and_then(|w| w.get("members")),
        "Cargo.toml [workspace].members",
    )?;
    let excluded = strings(
        workspace.and_then(|w| w.get("exclude")),
        "Cargo.toml [workspace].exclude",
    )?;
    for text in committed.members(&members, &excluded, "Cargo.toml")? {
        let member = toml_table("a workspace member's Cargo.toml", &text)?;
        if let Some(name) = string_at(&member, &["package", "name"]) {
            packages.insert(name.to_owned());
        }
    }
    Ok(())
}

fn npm_name(name: &str, packages: &mut BTreeSet<String>) {
    packages.insert(name.to_owned());
    if let Some((_, bare)) = name.strip_prefix('@').and_then(|rest| rest.split_once('/')) {
        packages.insert(bare.to_owned());
    }
}

fn npm(committed: &Committed<'_>, packages: &mut BTreeSet<String>) -> Result<(), Failure> {
    let Some(text) = committed.text("package.json")? else {
        return Ok(());
    };
    let root: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("package.json does not parse: {error}"))?;
    if let Some(name) = root.get("name").and_then(serde_json::Value::as_str) {
        npm_name(name, packages);
    }
    let workspaces = match root.get("workspaces") {
        None => Vec::new(),
        Some(serde_json::Value::Array(entries)) => entries.clone(),
        Some(serde_json::Value::Object(object)) => match object.get("packages") {
            None => Vec::new(),
            Some(serde_json::Value::Array(entries)) => entries.clone(),
            Some(_) => return Err("package.json workspaces.packages is not an array".to_owned()),
        },
        Some(_) => return Err("package.json workspaces is not an array or object".to_owned()),
    };
    let patterns = workspaces
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "package.json workspaces holds a non-string".to_owned())
        })
        .collect::<Result<Vec<String>, Failure>>()?;
    for text in committed.members(&patterns, &[], "package.json")? {
        let member: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("a workspace's package.json does not parse: {error}"))?;
        if let Some(name) = member.get("name").and_then(serde_json::Value::as_str) {
            npm_name(name, packages);
        }
    }
    Ok(())
}

fn python(committed: &Committed<'_>, packages: &mut BTreeSet<String>) -> Result<(), Failure> {
    let Some(text) = committed.text("pyproject.toml")? else {
        return Ok(());
    };
    let root = toml_table("pyproject.toml", &text)?;
    for path in [&["project", "name"][..], &["tool", "poetry", "name"][..]] {
        if let Some(name) = string_at(&root, path) {
            packages.insert(name.to_owned());
        }
    }
    Ok(())
}
