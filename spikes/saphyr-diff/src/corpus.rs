//! The documents the harness reads: the contract's YAML blocks, this repository's own
//! YAML, what the e2e suite fed the readers (captured by the crate's
//! `spike-saphyr-diff` dump), this host's installed files (read only), and synthetic
//! edge cases.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Rules,
    Releases,
    Workspaces,
    Branches,
    /// A file `rules apply` read as a base or an overlay.
    RulesApplyInput,
    /// The event-filter grammar the contract spells in YAML (read by tests only).
    Filter,
    /// `budgets.yaml`, walked schema-lessly by `tests/recovery-workload/churn.rs`.
    Budgets,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Rules => "rules",
            Kind::Releases => "releases",
            Kind::Workspaces => "workspaces",
            Kind::Branches => "branches",
            Kind::RulesApplyInput => "rules-apply-input",
            Kind::Filter => "filter",
            Kind::Budgets => "budgets",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub kind: Kind,
    /// `contract:N`, `repo:<file>`, `e2e:<digest>`, `host:<file>`, `synthetic:<name>`.
    pub source: String,
    pub text: String,
}

impl Doc {
    pub fn class(&self) -> &str {
        self.source.split(':').next().unwrap_or("")
    }
}

fn doc(kind: Kind, source: impl Into<String>, text: impl Into<String>) -> Doc {
    Doc {
        kind,
        source: source.into(),
        text: text.into(),
    }
}

/// The contract's ```yaml blocks, each classified by what it spells. A versionless
/// rules fragment gets the `version:` its contract test reads it under.
pub fn contract(repo: &Path) -> Vec<Doc> {
    let text = std::fs::read_to_string(repo.join("docs/contract.md")).expect("docs/contract.md");
    let mut out = Vec::new();
    let mut block: Option<String> = None;
    let mut n = 0;
    for line in text.lines() {
        match &mut block {
            None if line.trim_start() == "```yaml" => block = Some(String::new()),
            Some(body) if line.trim_start().starts_with("```") => {
                n += 1;
                let body = std::mem::take(body);
                block = None;
                let kind = if body.contains("repositories:") {
                    Kind::Releases
                } else if body.contains("pool:") {
                    Kind::Workspaces
                } else if body.contains("prefix:") && !body.contains("rules:") {
                    Kind::Branches
                } else if (body.contains("include:") || body.contains("exclude:"))
                    && !body.contains("rules:")
                {
                    Kind::Filter
                } else {
                    Kind::Rules
                };
                let body = if kind == Kind::Rules && !body.starts_with("version:") {
                    // contract.rs reads the drafts fragment under `version: 3`; the
                    // base_merge/checks fragment is read the same way here.
                    format!("version: 3\n{body}")
                } else {
                    body
                };
                out.push(doc(kind, format!("contract:{n}"), body));
            }
            Some(body) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {}
        }
    }
    out
}

pub fn repo_files(repo: &Path) -> Vec<Doc> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(repo.join("budgets.yaml")) {
        out.push(doc(Kind::Budgets, "repo:budgets.yaml", text));
    }
    out
}

/// Every document the crate's readers read during an e2e run, deduplicated by content.
pub fn e2e(dump: &Path) -> Vec<Doc> {
    let mut out = Vec::new();
    for (dir, kind) in [
        ("rules", Kind::Rules),
        ("releases", Kind::Releases),
        ("workspaces", Kind::Workspaces),
        ("branches", Kind::Branches),
        ("rules-apply-input", Kind::RulesApplyInput),
    ] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dump.join(dir))
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            out.push(doc(kind, format!("e2e:{name}"), text));
        }
    }
    out
}

/// What `rules apply` emitted during the e2e run (today's bytes).
pub fn e2e_emitted(dump: &Path) -> Vec<(String, String)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dump.join("rules-apply-emitted"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            (
                path.file_stem().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&path).unwrap_or_default(),
            )
        })
        .collect()
}

/// This host's installed files and the tracked copies `just repos-apply` installs,
/// read only.
pub fn host(onevcs_home: &Path, orchestrator: &Path) -> Vec<Doc> {
    let mut out = Vec::new();
    for (file, kind) in [
        ("rules.yml", Kind::Rules),
        ("releases.yml", Kind::Releases),
        ("workspaces.yml", Kind::Workspaces),
        ("branches.yml", Kind::Branches),
    ] {
        if let Ok(text) = std::fs::read_to_string(onevcs_home.join(file)) {
            out.push(doc(kind, format!("host:$ONEVCS_HOME/{file}"), text));
        }
    }
    for (file, kind) in [
        ("config/onevcs.rules.yml", Kind::Rules),
        ("config/onevcs.releases.yml", Kind::Releases),
        ("config/onevcs.workspaces.yml", Kind::Workspaces),
    ] {
        if let Ok(text) = std::fs::read_to_string(orchestrator.join(file)) {
            out.push(doc(kind, format!("host:ai-orchestrator/{file}"), text));
        }
    }
    if let Some(config) = std::env::var_os("HOME").map(PathBuf::from) {
        let overlay = config.join(".config/ai-orchestrator/workspaces.yml");
        if let Ok(text) = std::fs::read_to_string(&overlay) {
            out.push(doc(
                Kind::Workspaces,
                "host:~/.config/ai-orchestrator/workspaces.yml",
                text,
            ));
        }
    }
    out
}

/// Edge cases: the scalar spellings, key shapes and malformed documents where the two
/// crates could read differently.
pub fn synthetic() -> Vec<Doc> {
    let mut out = Vec::new();
    let mut add = |kind: Kind, name: &str, text: &str| out.push(doc(kind, format!("synthetic:{name}"), text));
    use Kind::*;
    let base = "default: {publication: change-open, approvals: required}\n";
    // Document shapes.
    add(Rules, "empty", "");
    add(Rules, "comment-only", "# nothing\n");
    add(Rules, "tilde", "~\n");
    add(Rules, "sequence", "- a\n- b\n");
    add(Rules, "scalar", "hello\n");
    add(Rules, "multi-document", "version: 4\n---\nversion: 4\n");
    add(Rules, "bom", "\u{feff}version: 4\ndefault: {publication: change-open, approvals: required}\n");
    add(Rules, "crlf", "version: 4\r\ndefault: {publication: change-open, approvals: required}\r\n");
    add(Rules, "syntax-error", "version: 4\ndefault: {publication: change-open\n");
    add(Rules, "tab-indent", "version: 4\ndefault:\n\tpublication: change-open\n");
    // Version spellings.
    for (name, v) in [
        ("version-quoted", "\"4\""),
        ("version-float", "4.0"),
        ("version-0", "0"),
        ("version-negative", "-1"),
        ("version-leading-zero", "04"),
        ("version-octal", "0o4"),
        ("version-hex", "0x4"),
        ("version-underscore", "0_4"),
        ("version-tilde", "~"),
        ("version-bool", "true"),
        ("version-huge", "99999999999999999999"),
        ("version-5", "5"),
    ] {
        add(Rules, name, &format!("version: {v}\n{base}"));
        add(Releases, name, &format!("version: {v}\ndefault: {{adoption: fast}}\nrepositories: []\n"));
        add(Workspaces, name, &format!("version: {v}\ndefault: {{pool: 0}}\n"));
        add(Branches, name, &format!("version: {v}\nprefix: nick/\n"));
    }
    add(Rules, "version-missing", base);
    add(Releases, "version-missing", "default: {adoption: fast}\nrepositories: []\n");
    add(Workspaces, "version-missing", "default: {pool: 0}\n");
    add(Branches, "version-missing", "prefix: nick/\n");
    // The e2e-asserted refusals.
    add(Releases, "missing-default", "version: 1\nrepositories: []\n");
    add(Rules, "missing-default", "version: 4\nrules: []\n");
    add(Rules, "unknown-top-level", &format!("version: 4\nfrobnicate: 1\n{base}"));
    add(Rules, "unknown-in-policy", "version: 4\ndefault: {publication: change-open, approvals: required, frobnicate: 1}\n");
    add(Rules, "unknown-in-match", &format!("version: 4\nrules:\n  - match: {{host: github.com, ownr: x}}\n    publication: local-direct\n{base}"));
    add(Rules, "bad-publication", "version: 4\ndefault: {publication: change-sometimes, approvals: required}\n");
    add(Rules, "bad-approvals", "version: 4\ndefault: {publication: change-open, approvals: maybe}\n");
    add(Rules, "gate-v2-default", "version: 2\ndefault: {publication: change-open, approvals: required, gate: {kind: checks}}\n");
    add(Rules, "gate-v2-rule", &format!("version: 2\nrules:\n  - match: {{path: \"~/p/*\"}}\n    publication: local-direct\n    gate: {{kind: pre-push}}\n{base}"));
    add(Rules, "gate-v3-default", "version: 3\ndefault: {publication: change-open, approvals: required, gate: {kind: checks}}\n");
    add(Rules, "gate-v4-rule", &format!("version: 4\nrules:\n  - match: {{path: \"~/p/*\"}}\n    publication: local-direct\n    gate: {{kind: pre-push}}\n{base}"));
    add(Rules, "gate-v3-nested-in-match", &format!("version: 3\nrules:\n  - match: {{path: \"~/p/*\", gate: x}}\n    publication: local-direct\n{base}"));
    add(Rules, "gate-null-v4", "version: 4\ndefault: {publication: change-open, approvals: required, gate: ~}\n");
    // drafts: the key and value spellings a refusal quotes.
    for (name, drafts) in [
        ("drafts-ok", "{disabled: true}"),
        ("drafts-not-mapping", "[disabled]"),
        ("drafts-scalar", "yes"),
        ("drafts-unknown-key", "{disabeld: true}"),
        ("drafts-yes", "{disabled: yes}"),
        ("drafts-on", "{disabled: on}"),
        ("drafts-True", "{disabled: True}"),
        ("drafts-quoted-true", "{disabled: \"true\"}"),
        ("drafts-int", "{disabled: 1}"),
        ("drafts-float", "{disabled: 1.50}"),
        ("drafts-null", "{disabled: ~}"),
        ("drafts-string", "{disabled: sometimes}"),
        ("drafts-quoted-string", "{disabled: 'it''s on'}"),
        ("drafts-seq", "{disabled: [a, b]}"),
        ("drafts-map", "{disabled: {a: 1}}"),
        ("drafts-multiline", "{disabled: \"a\\nb\"}"),
        ("drafts-int-key", "{1: true}"),
        ("drafts-bool-key", "{true: true}"),
        ("drafts-null-key", "{~: true}"),
        ("drafts-quoted-key", "{'disabled ': true}"),
        ("drafts-key-yes", "{yes: true}"),
        ("drafts-key-colon", "{\"a: b\": true}"),
        ("drafts-seq-key", "{? [a, b] : true}"),
        ("drafts-int-key-nonbool", "{1: 2}"),
        ("drafts-date", "{disabled: 2026-10-12}"),
    ] {
        add(Rules, name, &format!("version: 4\ndefault: {{publication: change-open, approvals: required, drafts: {drafts}}}\n"));
    }
    add(Rules, "drafts-in-rule", &format!("version: 4\nrules:\n  - match: {{name: x}}\n    drafts: {{disabled: maybe}}\n{base}"));
    for (name, value) in [
        ("base-merge-ok", "on-conflict"),
        ("base-merge-bad", "sometimes"),
        ("base-merge-yes", "yes"),
        ("base-merge-int", "1"),
        ("base-merge-null", "~"),
        ("base-merge-seq", "[always]"),
        ("base-merge-quoted", "\"always\""),
    ] {
        add(Rules, name, &format!("version: 4\ndefault: {{publication: change-open, approvals: required, base_merge: {value}}}\n"));
    }
    for (name, checks) in [
        ("checks-ok", "{wait_seconds: 3600}"),
        ("checks-float", "{wait_seconds: 1.5}"),
        ("checks-quoted", "{wait_seconds: \"3600\"}"),
        ("checks-underscore", "{wait_seconds: 3_600}"),
        ("checks-exp", "{wait_seconds: 1e3}"),
        ("checks-inf", "{wait_seconds: .inf}"),
        ("checks-nan", "{wait_seconds: .nan}"),
        ("checks-zero", "{wait_seconds: 0}"),
        ("checks-negative", "{wait_seconds: -5}"),
        ("checks-huge", "{wait_seconds: 1e12}"),
        ("checks-hex", "{wait_seconds: 0x10}"),
        ("checks-octal", "{wait_seconds: 0o10}"),
        ("checks-leading-zero", "{wait_seconds: 010}"),
        ("checks-null", "{wait_seconds: ~}"),
        ("checks-string", "{wait_seconds: soon}"),
        ("checks-unknown", "{wait: 5}"),
        ("checks-int-key", "{1: 5}"),
        ("checks-not-mapping", "5"),
    ] {
        add(Rules, name, &format!("version: 4\ndefault: {{publication: change-open, approvals: required, checks: {checks}}}\n"));
    }
    // String fields given other scalars.
    for (name, value) in [
        ("trailer-yes", "yes"),
        ("trailer-int", "42"),
        ("trailer-float", "1.10"),
        ("trailer-tilde", "~"),
        ("trailer-ok", "Onevcs-"),
    ] {
        add(Rules, name, &format!("version: 4\ntrailer_prefix: {value}\n{base}"));
    }
    for (name, value) in [
        ("match-name-yes", "yes"),
        ("match-name-no", "no"),
        ("match-name-on", "on"),
        ("match-name-int", "1234"),
        ("match-name-float", "1.10"),
        ("match-name-underscore", "1_000"),
        ("match-name-leading-zero", "007"),
        ("match-name-null", "null"),
        ("match-name-date", "2026-10-12"),
        ("match-name-octal-ish", "0o17"),
        ("match-name-sexagesimal", "1:20"),
    ] {
        add(Rules, name, &format!("version: 4\nrules:\n  - match: {{host: github.com, owner: o, name: {value}}}\n    publication: local-direct\n{base}"));
        add(Branches, &name.replace("match-name", "prefix"), &format!("version: 1\nprefix: {value}\n"));
    }
    add(Rules, "visibility-yes", &format!("version: 4\nrules:\n  - match: {{name: x}}\n    visibility: yes\n{base}"));
    // Anchors, merge keys, duplicates, tags, non-string keys.
    add(Rules, "anchor-alias", "version: 4\nx: &p {publication: change-open, approvals: required}\ndefault: *p\n");
    add(Rules, "merge-key", "version: 4\nx: &p {publication: change-open}\ndefault:\n  <<: *p\n  approvals: required\n");
    add(Rules, "merge-key-override", "version: 4\nx: &p {publication: change-open, approvals: none}\ndefault:\n  <<: *p\n  approvals: required\n");
    add(Rules, "merge-list", "version: 4\na: &a {publication: change-open}\nb: &b {approvals: required}\ndefault:\n  <<: [*a, *b]\n");
    add(Rules, "merge-null", "version: 4\ndefault:\n  <<: ~\n  publication: change-open\n  approvals: required\n");
    add(Rules, "merge-quoted", "version: 4\ndefault:\n  '<<': {publication: change-open}\n  approvals: required\n");
    add(Rules, "merge-into-drafts", "version: 4\nd: &d {disabled: true}\ndefault:\n  publication: change-open\n  approvals: required\n  drafts:\n    <<: *d\n");
    add(Rules, "duplicate-top", &format!("version: 4\nversion: 4\n{base}"));
    add(Rules, "duplicate-in-policy", "version: 4\ndefault: {publication: change-open, publication: local-direct, approvals: required}\n");
    add(Rules, "duplicate-unknown", &format!("version: 4\nx: 1\nx: 2\n{base}"));
    add(Rules, "duplicate-in-drafts", "version: 4\ndefault: {publication: change-open, approvals: required, drafts: {disabled: true, disabled: false}}\n");
    add(Rules, "tag-str-version", &format!("version: !!str 4\n{base}"));
    add(Rules, "tag-local", &format!("version: 4\nx: !custom 1\n{base}"));
    add(Rules, "tag-on-enum", "version: 4\ndefault: {publication: !local change-open, approvals: required}\n");
    add(Rules, "int-key-top", &format!("version: 4\n1: one\n{base}"));
    add(Rules, "bool-key-top", &format!("version: 4\ntrue: one\n{base}"));
    add(Rules, "null-key-top", &format!("version: 4\n~: one\n{base}"));
    add(Rules, "complex-key-top", &format!("version: 4\n? [a, b]\n: one\n{base}"));
    add(Rules, "deep-nesting", &format!("version: 4\nx: {}{}\n{base}", "[".repeat(200), "]".repeat(200)));
    add(Rules, "block-scalar-trailer", &format!("version: 4\ntrailer_prefix: |\n  Onevcs-\n{base}"));
    // Releases-specific scalars.
    add(Releases, "probe-timeout-quoted", "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: x}\n    targets:\n      - {name: crate, style: automated, probe: {shell: 'true', timeout_seconds: \"60\"}}\n");
    add(Releases, "probe-timeout-float", "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: x}\n    targets:\n      - {name: crate, style: automated, probe: {shell: 'true', timeout_seconds: 60.0}}\n");
    add(Releases, "probe-args-mixed", "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: x}\n    targets:\n      - {name: crate, style: automated, probe: {script: s.sh, args: [1, yes, ~, 1.10]}}\n");
    add(Releases, "target-name-int", "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: x}\n    targets:\n      - {name: 1, style: human-step, action: do it}\n");
    add(Releases, "action-yes", "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: x}\n    targets:\n      - {name: crate, style: human-step, action: yes}\n");
    add(Releases, "unknown-key", "version: 1\ndefault: {adoption: fast, frobnicate: 1}\nrepositories: []\n");
    add(Releases, "bad-adoption", "version: 1\ndefault: {adoption: eventually}\nrepositories: []\n");
    add(Releases, "duplicate-target", "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: x}\n    targets:\n      - {name: crate, style: human-step, action: a}\n      - {name: crate, style: human-step, action: b}\n");
    // Workspaces-specific scalars.
    for (name, body) in [
        ("pool-quoted", "default: {pool: \"2\"}"),
        ("pool-float", "default: {pool: 2.0}"),
        ("pool-negative", "default: {pool: -1}"),
        ("overflow-unlimited", "default: {pool: 1, overflow: unlimited}"),
        ("overflow-Unlimited", "default: {pool: 1, overflow: Unlimited}"),
        ("overflow-quoted-int", "default: {pool: 1, overflow: \"3\"}"),
        ("overflow-yes", "default: {pool: 1, overflow: yes}"),
        ("delete-mixed", "default: {pool: 1, delete: [.logs/, 1, yes]}"),
        ("delete-escape", "default: {pool: 1, delete: [../x]}"),
        ("maintain-timeout-int", "default: {pool: 1, maintain: {command: [cargo, sweep], timeout: 30}}"),
        ("maintain-timeout-span", "default: {pool: 1, maintain: {command: [cargo, sweep, --time, 7], timeout: 30m}}"),
        ("maintain-command-empty", "default: {pool: 1, maintain: {command: [], timeout: 30m}}"),
        ("maintain-timeout-sexagesimal", "default: {pool: 1, maintain: {command: [x], timeout: 1:30}}"),
        ("pool-zero-overflow-zero", "default: {pool: 0, overflow: 0}"),
        ("unknown-key", "default: {pool: 0, frobnicate: 1}"),
    ] {
        add(Workspaces, name, &format!("version: 1\n{body}\n"));
    }
    add(Branches, "prefix-empty", "version: 1\nprefix: \"\"\n");
    add(Branches, "prefix-missing", "version: 1\n");
    add(Branches, "unknown-key", "version: 1\nprefix: nick/\nsuffix: x\n");
    out
}

/// The overlays `rules apply` is exercised with beyond the e2e capture: the unknown-key
/// diff over non-string keys and nulls.
pub fn apply_overlays() -> Vec<(String, String)> {
    [
        ("ok", "version: 4\nrules:\n  - match: {host: github.com, owner: spike, name: x}\n    publication: local-direct\n"),
        ("null-unknown", "version: 4\nfrobnicate: ~\n"),
        ("null-known-nested", "version: 4\ndefault: {drafts: ~}\n"),
        ("unknown-top", "version: 4\nfrobnicate: 1\n"),
        ("unknown-nested", "version: 4\ndefault: {drafts: {disabled: true, frob: 1}}\n"),
        ("unknown-in-rule", "version: 4\nrules:\n  - match: {name: x, frob: 1}\n    publication: local-direct\n"),
        ("int-key-top", "version: 4\n1: one\n"),
        ("bool-key-top", "version: 4\ntrue: one\n"),
        ("null-key-top", "version: 4\n~: one\n"),
        ("float-key-top", "version: 4\n1.5: one\n"),
        ("int-key-null-value", "version: 4\n1: ~\n"),
        ("int-key-nested", "version: 4\nrules:\n  - match: {name: x, 7: y}\n    publication: local-direct\n"),
        ("seq-key", "version: 4\n? [a, b]\n: one\n"),
        ("gate-in-overlay", "version: 4\ndefault: {gate: {kind: checks}}\n"),
        ("unknown-in-default-null", "version: 4\ndefault: {publication: ~, frob: ~}\n"),
        ("yes-value", "version: 4\nrules:\n  - match: {name: yes}\n    publication: local-direct\n"),
        ("version-quoted", "version: \"4\"\n"),
    ]
    .into_iter()
    .map(|(n, t)| (n.to_owned(), t.to_owned()))
    .collect()
}

/// Every literal a test asserts a message contains, from the e2e and contract sources.
pub fn asserted_substrings(repo: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut files = vec![repo.join("crates/onevcs/tests/contract.rs")];
    if let Ok(entries) = std::fs::read_dir(repo.join("crates/onevcs/tests/e2e")) {
        files.extend(entries.flatten().map(|e| e.path()));
    }
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for (index, _) in text.match_indices("contains(\"") {
            let rest = &text[index + "contains(\"".len()..];
            let mut literal = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => match chars.next() {
                        Some('n') => literal.push('\n'),
                        Some('"') => literal.push('"'),
                        Some('\\') => literal.push('\\'),
                        Some(other) => {
                            literal.push('\\');
                            literal.push(other)
                        }
                        None => break,
                    },
                    '"' => break,
                    other => literal.push(other),
                }
            }
            if literal.len() >= 6 {
                found.insert(literal);
            }
        }
    }
    found
}
