//! Differential harness (spike-onevcs-yaml; never lands): this repository's YAML
//! readers and writers as they are today (serde_yaml_ng 0.10) against the same code
//! ported to serde-saphyr 1.3 — with `serde_json::Value` (`preserve_order`) or
//! `saphyr` 0.2's node tree for the schema-less walks — over the contract's YAML
//! blocks, this repository's own YAML, what the e2e suite fed the readers, this
//! host's installed files (read only), and synthetic edge cases. Prints every
//! difference in what is read, what is written and what errors say.
//!
//! Run it with `spikes/saphyr-diff/run.sh`.

mod corpus;
mod port;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use corpus::{Doc, Kind};
use onevcs::branches::BranchesFile;
use onevcs::releases::ReleasesFile;
use onevcs::rules::RulesFile;
use onevcs::spike::Overlay;
use onevcs::workspaces::WorkspacesFile;
use port::{Flavor, FLAVORS};
use serde::Serialize;
use serde_json::Value as J;

/// What one reader made of one document: the typed value as canonical JSON, or the
/// refusal's text.
type Outcome = Result<String, String>;

struct Ctx {
    home: PathBuf,
    asserted: std::collections::BTreeSet<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
    };
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = arg("--repo").unwrap_or_else(|| manifest.join("../.."));
    let repo = repo.canonicalize().expect("the repository");
    let dump = arg("--dump").unwrap_or_else(|| repo.join("target/saphyr-diff/e2e-dump"));
    let home_dir = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let onevcs_home = arg("--onevcs-home").unwrap_or_else(|| home_dir.join(".onevcs"));
    let orchestrator =
        arg("--orchestrator").unwrap_or_else(|| home_dir.join("projects/ai-orchestrator"));
    let iterations: usize = args
        .iter()
        .position(|a| a == "--iterations")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(2000);

    let scratch = std::env::temp_dir().join(format!("saphyr-diff-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    // Every host-file reader reads $ONEVCS_HOME; point it at a scratch root so this
    // host's own state is never written (the host files are read, copied, and only
    // the copy is handed to a reader).
    std::env::set_var("ONEVCS_HOME", &scratch);
    let ctx = Ctx { home: scratch.clone(), asserted: corpus::asserted_substrings(&repo) };

    let mut docs: Vec<Doc> = Vec::new();
    docs.extend(corpus::contract(&repo));
    docs.extend(corpus::repo_files(&repo));
    let e2e = corpus::e2e(&dump);
    docs.extend(e2e);
    docs.extend(corpus::host(&onevcs_home, &orchestrator));
    docs.extend(corpus::synthetic());

    println!("# saphyr-diff: serde_yaml_ng 0.10.0 (today) vs serde-saphyr 1.3.0 / saphyr 0.2.1");
    println!("# repo {}  e2e dump {}  host {}", repo.display(), dump.display(), onevcs_home.display());
    section_corpus(&docs, &dump);
    let reads = section_reads(&ctx, &docs);
    section_asserted(&ctx, &reads);
    section_rules_apply(&ctx, &docs, &orchestrator);
    section_writes(&ctx, &docs, &dump);
    section_schemaless(&docs);
    section_timing(&docs, iterations);
    let _ = std::fs::remove_dir_all(&scratch);
}

// ------------------------------------------------------------------- corpus

fn section_corpus(docs: &[Doc], dump: &Path) {
    println!("\n== corpus (documents per kind and source class)");
    let mut counts: BTreeMap<(Kind, String), usize> = BTreeMap::new();
    for doc in docs {
        *counts.entry((doc.kind, doc.class().to_owned())).or_default() += 1;
    }
    for ((kind, class), n) in &counts {
        println!("  {:<18} {:<10} {n}", kind.name(), class);
    }
    println!("  rules-apply emissions captured from the e2e run: {}", corpus::e2e_emitted(dump).len());
}

// -------------------------------------------------------------------- reads

fn normalize(text: &str, ctx: &Ctx) -> String {
    text.replace(&ctx.home.display().to_string(), "$H")
}

fn outcome<T: Serialize>(result: Result<T, onevcs::Error>, ctx: &Ctx) -> Outcome {
    result
        .map(|typed| serde_json::to_string(&typed).expect("a typed value serializes"))
        .map_err(|error| normalize(&error.to_string(), ctx))
}

fn host_path(ctx: &Ctx, kind: Kind) -> PathBuf {
    ctx.home.join(match kind {
        Kind::Releases => "releases.yml",
        Kind::Workspaces => "workspaces.yml",
        Kind::Branches => "branches.yml",
        _ => "rules.yml",
    })
}

/// Today's reader over one document.
fn today(ctx: &Ctx, kind: Kind, text: &str) -> Vec<(String, Outcome)> {
    let path = host_path(ctx, kind);
    match kind {
        Kind::Rules => vec![("".into(), outcome(onevcs::spike::rules_parse(&path, text), ctx))],
        Kind::Releases => {
            std::fs::write(&path, text).unwrap();
            vec![("".into(), outcome(onevcs::spike::releases_load(), ctx))]
        }
        Kind::Workspaces => {
            std::fs::write(&path, text).unwrap();
            vec![("".into(), outcome(onevcs::spike::workspaces_load(), ctx))]
        }
        Kind::Branches => {
            std::fs::write(&path, text).unwrap();
            vec![("".into(), outcome(onevcs::spike::branches_load(), ctx))]
        }
        Kind::RulesApplyInput => vec![
            (
                "as base".into(),
                outcome(
                    onevcs::spike::strictly::<RulesFile>(&path, text)
                        .and_then(|_| onevcs::spike::rules_parse(&path, text)),
                    ctx,
                ),
            ),
            ("as overlay".into(), outcome(onevcs::spike::strictly::<Overlay>(&path, text), ctx)),
        ],
        Kind::Filter => vec![(
            "".into(),
            serde_yaml_ng::from_str::<onevcs::EventFilter>(text)
                .map(|f| serde_json::to_string(&f).unwrap())
                .map_err(|e| e.to_string()),
        )],
        Kind::Budgets | Kind::Workflow => vec![(
            "".into(),
            serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
                .map(|v| serde_json::to_string(&yaml_ng_to_json(&v)).unwrap())
                .map_err(|e| e.to_string()),
        )],
    }
}

/// The serde-saphyr port of the same reader over the same document.
fn ported(ctx: &Ctx, kind: Kind, text: &str, f: &Flavor) -> Vec<(String, Outcome)> {
    let path = host_path(ctx, kind);
    match kind {
        Kind::Rules => vec![("".into(), outcome(port::rules(&path, text, f), ctx))],
        Kind::Releases => vec![(
            "".into(),
            outcome(
                port::host_file::<ReleasesFile>("release-targets", &path, text, f, |p, v| {
                    onevcs::spike::releases_validate(p, v)
                }),
                ctx,
            ),
        )],
        Kind::Workspaces => vec![(
            "".into(),
            outcome(
                port::host_file::<WorkspacesFile>("workspaces", &path, text, f, |p, v| {
                    onevcs::spike::workspaces_validate(p, v)
                }),
                ctx,
            ),
        )],
        Kind::Branches => vec![(
            "".into(),
            outcome(port::host_file::<BranchesFile>("branches", &path, text, f, |_, _| Ok(())), ctx),
        )],
        Kind::RulesApplyInput => vec![
            (
                "as base".into(),
                outcome(
                    port::strictly_json::<RulesFile>(&path, text, f)
                        .and_then(|_| port::rules(&path, text, f)),
                    ctx,
                ),
            ),
            ("as overlay".into(), outcome(port::strictly_json::<Overlay>(&path, text, f), ctx)),
        ],
        Kind::Filter => vec![(
            "".into(),
            serde_saphyr::from_str_with_options::<onevcs::EventFilter>(text, f.options())
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| e.to_string()),
        )],
        Kind::Budgets | Kind::Workflow => vec![(
            "".into(),
            f.tree(text).map(|v| serde_json::to_string(&v).unwrap()),
        )],
    }
}

struct Read {
    doc: Doc,
    today: Vec<(String, Outcome)>,
    ported: Vec<(&'static str, Vec<(String, Outcome)>)>,
}

fn show(o: &Outcome) -> String {
    match o {
        Ok(v) => format!("ok {}", clip(v, 400)),
        Err(e) => format!("ERR {}", clip(e, 1200)),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_owned()
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…[{} bytes]", &s[..end], s.len())
    }
}

fn indent(s: &str, by: &str) -> String {
    s.lines().map(|l| format!("{by}{l}")).collect::<Vec<_>>().join("\n")
}

fn section_reads(ctx: &Ctx, docs: &[Doc]) -> Vec<Read> {
    println!("\n== reads: today vs each serde-saphyr flavor (\"= today\" means identical)");
    println!("   flavors: {}", FLAVORS.iter().map(|f| f.name).collect::<Vec<_>>().join(", "));
    let mut reads = Vec::new();
    for doc in docs {
        let today = today(ctx, doc.kind, &doc.text);
        let ported = FLAVORS.iter().map(|f| (f.name, ported(ctx, doc.kind, &doc.text, f))).collect();
        reads.push(Read { doc: doc.clone(), today, ported });
    }
    let mut kinds: Vec<Kind> = reads.iter().map(|r| r.doc.kind).collect();
    kinds.sort();
    kinds.dedup();
    for kind in kinds {
        let of_kind: Vec<&Read> = reads.iter().filter(|r| r.doc.kind == kind).collect();
        println!("\n--- {} ({} documents)", kind.name(), of_kind.len());
        for (fi, f) in FLAVORS.iter().enumerate() {
            let differing = of_kind.iter().filter(|r| r.ported[fi].1 != r.today).count();
            let classes = |pred: &dyn Fn(&Outcome, &Outcome) -> bool| {
                of_kind
                    .iter()
                    .flat_map(|r| r.today.iter().zip(&r.ported[fi].1))
                    .filter(|((_, a), (_, b))| a != b && pred(a, b))
                    .count()
            };
            println!(
                "    {:<52} differing docs {:>3}  [ok→ok value {} | ok→err {} | err→ok {} | err→err text {}]",
                f.name,
                differing,
                classes(&|a, b| a.is_ok() && b.is_ok()),
                classes(&|a, b| a.is_ok() && b.is_err()),
                classes(&|a, b| a.is_err() && b.is_ok()),
                classes(&|a, b| a.is_err() && b.is_err()),
            );
        }
        for r in &of_kind {
            if r.ported.iter().all(|(_, p)| *p == r.today) {
                continue;
            }
            println!("\n  [{}] {}", kind.name(), r.doc.source);
            if r.doc.text.len() <= 360 {
                println!("{}", indent(r.doc.text.trim_end(), "    | "));
            } else {
                println!("    | ({} bytes)", r.doc.text.len());
            }
            for (i, (label, t)) in r.today.iter().enumerate() {
                let label = if label.is_empty() { String::new() } else { format!(" ({label})") };
                println!("    today{label}: {}", show(t));
                let mut seen: Vec<(&str, &Outcome)> = Vec::new();
                for (name, p) in &r.ported {
                    let o = &p[i].1;
                    if o == t {
                        println!("      {name}: = today");
                    } else if let Some((earlier, _)) = seen.iter().find(|(_, e)| *e == o) {
                        println!("      {name}: = {earlier}");
                    } else {
                        println!("      {name}: {}", show(o));
                    }
                    seen.push((name, o));
                }
            }
        }
    }
    reads
}

/// The literals a test asserts a refusal contains: does each refusal that carried one
/// today still carry it under each flavor?
fn section_asserted(ctx: &Ctx, reads: &[Read]) {
    let _ = ctx;
    println!("\n== asserted refusal substrings (literals the e2e and contract suites assert a message contains)");
    let mut lost: BTreeMap<(&str, String), Vec<String>> = BTreeMap::new();
    let mut carried = 0;
    for r in reads {
        for (i, (_, t)) in r.today.iter().enumerate() {
            let Err(today) = t else { continue };
            for needle in ctx.asserted.iter().filter(|n| today.contains(n.as_str())) {
                carried += 1;
                for (name, p) in &r.ported {
                    let keeps = matches!(&p[i].1, Err(e) if e.contains(needle.as_str()));
                    if !keeps {
                        lost.entry((name, needle.clone())).or_default().push(r.doc.source.clone());
                    }
                }
            }
        }
    }
    println!("  (refusal, asserted literal) pairs today: {carried}");
    if lost.is_empty() {
        println!("  every flavor keeps every asserted literal");
    }
    for ((flavor, needle), sources) in &lost {
        println!("  {flavor}: loses {needle:?} in {} docs, e.g. {}", sources.len(), sources[0]);
    }
}

// --------------------------------------------- rules apply: the unknown-key walk

fn section_rules_apply(ctx: &Ctx, docs: &[Doc], orchestrator: &Path) {
    println!("\n== rules apply: the unknown-key walk (`strictly`), today vs serde_json::Value vs saphyr's tree");
    let path = ctx.home.join("overlay.yml");
    let mut overlays = corpus::apply_overlays();
    for doc in docs.iter().filter(|d| d.kind == Kind::RulesApplyInput) {
        overlays.push((doc.source.clone(), doc.text.clone()));
    }
    let f = &FLAVORS[5];
    let g = &FLAVORS[8];
    let mut same = 0;
    for (name, text) in &overlays {
        let today = outcome(onevcs::spike::strictly::<Overlay>(&path, text), ctx);
        let json = outcome(port::strictly_json::<Overlay>(&path, text, f), ctx);
        let tree = outcome(port::strictly_saphyr::<Overlay>(&path, text, g), ctx);
        if json == today && tree == today {
            same += 1;
            continue;
        }
        println!("\n  [overlay] {name}");
        println!("{}", indent(text.trim_end(), "    | "));
        println!("    today:                {}", show(&today));
        println!("    json ({}): {}", f.name, if json == today { "= today".into() } else { show(&json) });
        println!("    saphyr tree ({}): {}", g.name, if tree == today { "= today".into() } else { show(&tree) });
    }
    println!("\n  overlays identical under both walks: {same} of {}", overlays.len());
    let _ = orchestrator;
}

// -------------------------------------------------------------------- writes

#[derive(Clone)]
struct SerFlavor {
    name: &'static str,
    options: fn() -> serde_saphyr::SerializerOptions,
}

const SER_FLAVORS: &[SerFlavor] = &[
    SerFlavor { name: "default", options: || serde_saphyr::ser_options! {} },
    SerFlavor { name: "compact_list_indent=false", options: || serde_saphyr::ser_options! { compact_list_indent: false } },
    SerFlavor { name: "compact_list_indent=false,prefer_block_scalars=false", options: || serde_saphyr::ser_options! { compact_list_indent: false, prefer_block_scalars: false } },
    SerFlavor { name: "compact_list_indent=false,prefer_block_scalars=false,empty_as_braces=false", options: || serde_saphyr::ser_options! { compact_list_indent: false, prefer_block_scalars: false, empty_as_braces: false } },
    SerFlavor { name: "compact_list_indent=false,yaml_12", options: || serde_saphyr::ser_options! { compact_list_indent: false, yaml_12: true } },
];

fn emit_today<T: Serialize>(value: &T) -> String {
    serde_yaml_ng::to_string(value).unwrap_or_else(|e| format!("<error: {e}>"))
}

fn emit_saphyr<T: Serialize>(value: &T, f: &SerFlavor) -> String {
    serde_saphyr::to_string_with_options(value, (f.options)()).unwrap_or_else(|e| format!("<error: {e}>"))
}

/// A line diff (LCS), `-` today and `+` serde-saphyr.
fn diff(a: &str, b: &str) -> String {
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push(format!("+{}", b[j]));
            j += 1;
        } else {
            out.push(format!("-{}", a[i]));
            i += 1;
        }
    }
    out.join("\n")
}

fn section_writes(ctx: &Ctx, docs: &[Doc], dump: &Path) {
    println!("\n== writes");
    let path = ctx.home.join("rules.yml");
    let header = onevcs::spike::RULES_APPLY_HEADER;

    // The composed rules.yml `rules apply` installs and prints, for this host's
    // tracked rules file as the base (what `just repos-apply` installs, by copy),
    // with no overlay and with the corpus's one well-formed overlay.
    for doc in docs.iter().filter(|d| d.kind == Kind::Rules && d.source.starts_with("host:ai-orchestrator")) {
        let base = &doc.text;
        let overlay_path = ctx.home.join("overlay.yml");
        let overlay_text = &corpus::apply_overlays()[0].1;
        std::fs::write(ctx.home.join("base.yml"), base).unwrap();
        std::fs::write(&overlay_path, overlay_text).unwrap();
        for overlays in [vec![], vec![overlay_path.clone()]] {
            let label = format!("rules apply --base {}{}", doc.source, if overlays.is_empty() { "" } else { " --overlay corpus:ok" });
            let real = onevcs::rules_apply(&onevcs::RulesApplyRequest {
                base: ctx.home.join("base.yml"),
                overlays: overlays.clone(),
                dry_run: true,
            });
            let Ok(applied) = real else {
                println!("\n  [{label}] today's rules apply refused: {}", real.unwrap_err());
                continue;
            };
            let mirrored = format!("{header}{}", emit_today(&applied.rules));
            println!("\n  [{label}] the real `rules_apply` document equals HEADER + serde_yaml_ng::to_string(composed): {}", mirrored == applied.document);
            for f in SER_FLAVORS {
                let saphyr = format!("{header}{}", emit_saphyr(&applied.rules, f));
                let same = saphyr == applied.document;
                println!("    serializer options {}: {}", f.name, if same { "byte-identical".to_owned() } else { format!("differs in {} lines", diff(&applied.document, &saphyr).lines().count()) });
                if !same && f.name == "default" || !same && f.name == "compact_list_indent=false" {
                    println!("{}", indent(&diff(&applied.document, &saphyr), "      "));
                }
                let reread = onevcs::spike::rules_parse(&path, &saphyr);
                println!(
                    "      today's reader reads it back to the same rules: {}",
                    match reread { Ok(r) if r == applied.rules => "yes".to_owned(), Ok(_) => "NO (different)".to_owned(), Err(e) => format!("NO ({e})") }
                );
            }
        }
    }

    // What the real binary emitted during the e2e run.
    let emitted = corpus::e2e_emitted(dump);
    let mut tallies = vec![0usize; SER_FLAVORS.len()];
    let mut shown = vec![false; SER_FLAVORS.len()];
    let mut read = 0;
    for (name, text) in &emitted {
        let Ok(rules) = onevcs::spike::rules_parse(&path, text) else { continue };
        read += 1;
        let today = format!("{header}{}", emit_today(&rules));
        assert_eq!(&today, text, "serde_yaml_ng re-emission of e2e emission {name} is not what the binary wrote");
        for (i, f) in SER_FLAVORS.iter().enumerate() {
            let saphyr = format!("{header}{}", emit_saphyr(&rules, f));
            if saphyr == today {
                tallies[i] += 1;
            } else if !shown[i] && i <= 1 {
                shown[i] = true;
                println!("\n  [e2e rules apply emission e2e:{name}] serializer options {} differs:", f.name);
                println!("{}", indent(&diff(&today, &saphyr), "    "));
            }
        }
    }
    println!("\n  e2e `rules apply` emissions: {} captured, {} re-read; each equals today's serde_yaml_ng re-emission", emitted.len(), read);
    for (i, f) in SER_FLAVORS.iter().enumerate() {
        println!("    byte-identical under {}: {}/{}", f.name, tallies[i], read);
    }

    println!("\n  [BranchesFile::default()]");
    println!("    today:        {:?}", emit_today(&BranchesFile::default()));
    for f in SER_FLAVORS {
        println!("    {:<80} {:?}", f.name, emit_saphyr(&BranchesFile::default(), f));
    }

    // Every value read from the corpus, written back by both crates.
    println!("\n  every document today reads, written back by each crate (byte-identical count per serializer options)");
    let mut per_kind: BTreeMap<&str, (usize, Vec<usize>)> = BTreeMap::new();
    let mut first_diff: BTreeMap<(&str, usize), String> = BTreeMap::new();
    let mut tally = |kind: &'static str, source: &str, today: String, saphyrs: Vec<String>| {
        let entry = per_kind.entry(kind).or_insert((0, vec![0; SER_FLAVORS.len()]));
        entry.0 += 1;
        for (i, s) in saphyrs.iter().enumerate() {
            if *s == today {
                entry.1[i] += 1;
            } else {
                first_diff.entry((kind, i)).or_insert_with(|| format!("{source}\n{}", diff(&today, s)));
            }
        }
    };
    for doc in docs {
        let saphyrs = |v: &dyn Fn(&SerFlavor) -> String| SER_FLAVORS.iter().map(v).collect::<Vec<_>>();
        match doc.kind {
            Kind::Rules => {
                if let Ok(v) = onevcs::spike::rules_parse(&path, &doc.text) {
                    tally("rules", &doc.source, emit_today(&v), saphyrs(&|f| emit_saphyr(&v, f)));
                }
            }
            Kind::Releases | Kind::Workspaces | Kind::Branches => {
                std::fs::write(host_path(ctx, doc.kind), &doc.text).unwrap();
                match doc.kind {
                    Kind::Releases => {
                        if let Ok(v) = onevcs::spike::releases_load() {
                            tally("releases", &doc.source, emit_today(&v), saphyrs(&|f| emit_saphyr(&v, f)));
                        }
                    }
                    Kind::Workspaces => {
                        if let Ok(v) = onevcs::spike::workspaces_load() {
                            tally("workspaces", &doc.source, emit_today(&v), saphyrs(&|f| emit_saphyr(&v, f)));
                        }
                    }
                    _ => {
                        if let Ok(v) = onevcs::spike::branches_load() {
                            tally("branches", &doc.source, emit_today(&v), saphyrs(&|f| emit_saphyr(&v, f)));
                        }
                    }
                }
            }
            Kind::Filter => {
                if let Ok(v) = serde_yaml_ng::from_str::<onevcs::EventFilter>(&doc.text) {
                    tally("filter", &doc.source, emit_today(&v), saphyrs(&|f| emit_saphyr(&v, f)));
                }
            }
            _ => {}
        }
    }
    for (kind, (n, counts)) in &per_kind {
        let summary: Vec<String> = SER_FLAVORS.iter().zip(counts).map(|(f, c)| format!("{}={c}/{n}", f.name)).collect();
        println!("    {kind}: {}", summary.join("  "));
    }
    for ((kind, i), example) in &first_diff {
        if *i > 1 {
            continue;
        }
        println!("\n    first {kind} difference under {}: {}", SER_FLAVORS[*i].name, indent(example, "      ").trim_start());
    }
}

// ------------------------------------------------------------- schema-less trees

/// serde_yaml_ng's tree as JSON for comparison: a non-string key is its YAML spelling,
/// a tag is `{"!tag": value}`.
fn yaml_ng_to_json(v: &serde_yaml_ng::Value) -> J {
    use serde_yaml_ng::Value as Y;
    match v {
        Y::Null => J::Null,
        Y::Bool(b) => J::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                J::from(i)
            } else if let Some(u) = n.as_u64() {
                J::from(u)
            } else {
                serde_json::Number::from_f64(n.as_f64().unwrap()).map(J::Number).unwrap_or_else(|| J::String(n.to_string()))
            }
        }
        Y::String(s) => J::String(s.clone()),
        Y::Sequence(s) => J::Array(s.iter().map(yaml_ng_to_json).collect()),
        Y::Mapping(m) => J::Object(
            m.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Y::String(s) => s.clone(),
                        other => serde_yaml_ng::to_string(other).unwrap_or_default().trim().to_owned(),
                    };
                    (key, yaml_ng_to_json(v))
                })
                .collect(),
        ),
        Y::Tagged(t) => {
            let mut m = serde_json::Map::new();
            m.insert(t.tag.to_string(), yaml_ng_to_json(&t.value));
            J::Object(m)
        }
    }
}

fn section_schemaless(docs: &[Doc]) {
    println!("\n== schema-less trees: what each tree carries");
    let probes = [
        ("int key", "1: a\n"),
        ("bool key", "true: a\n"),
        ("null key", "~: a\n"),
        ("float key", "1.5: a\n"),
        ("sequence key", "? [a, b]\n: a\n"),
        ("int key, null value", "1: ~\n"),
        ("`on:` key (workflows)", "on: push\n"),
        ("`yes` value", "a: yes\n"),
        ("`0o17` value", "a: 0o17\n"),
        ("`017` value", "a: 017\n"),
        ("`0x1F` value", "a: 0x1F\n"),
        ("`1_000` value", "a: 1_000\n"),
        ("`1:20` value", "a: 1:20\n"),
        ("`.inf` value", "a: .inf\n"),
        ("`.nan` value", "a: .nan\n"),
        ("`2026-10-12` value", "a: 2026-10-12\n"),
        ("`!custom 1` value", "a: !custom 1\n"),
        ("`!!str 4` value", "a: !!str 4\n"),
        ("`1e3` value", "a: 1e3\n"),
        ("`99999999999999999999` value", "a: 99999999999999999999\n"),
        ("merge key", "x: &p {b: 1}\na:\n  <<: *p\n  c: 2\n"),
        ("duplicate key", "a: 1\na: 2\n"),
        ("empty document", ""),
    ];
    use saphyr::LoadableYamlNode;
    for (name, text) in probes {
        let today = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
            .map(|v| format!("{v:?} → as JSON {}", serde_json::to_string(&yaml_ng_to_json(&v)).unwrap()))
            .map_err(|e| e.to_string());
        let json = FLAVORS[0].tree(text).map(|v| serde_json::to_string(&v).unwrap());
        let json_strict = FLAVORS[4].tree(text).map(|v| serde_json::to_string(&v).unwrap());
        let tree = saphyr::Yaml::load_from_str(text)
            .map(|docs| docs.first().map(|d| format!("{d:?}")).unwrap_or_else(|| "(no document)".into()))
            .map_err(|e| e.to_string());
        println!("\n  {name}: {text:?}");
        println!("    serde_yaml_ng::Value: {}", match &today { Ok(v) => v.clone(), Err(e) => format!("ERR {}", clip(e, 300)) });
        println!("    serde_json::Value via serde-saphyr (default): {}", match &json { Ok(v) => v.clone(), Err(e) => format!("ERR {}", clip(e, 300)) });
        println!("    serde_json::Value via serde-saphyr (strict_booleans, merge as ordinary, non-finite ok): {}", match &json_strict { Ok(v) => v.clone(), Err(e) => format!("ERR {}", clip(e, 300)) });
        println!("    saphyr::Yaml: {}", match &tree { Ok(v) => clip(v, 300), Err(e) => format!("ERR {e}") });
    }

    println!("\n  real schema-less documents (budgets.yaml, workflows): today's tree as JSON vs serde-saphyr's serde_json::Value");
    for doc in docs.iter().filter(|d| matches!(d.kind, Kind::Budgets | Kind::Workflow)) {
        let today = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&doc.text).map(|v| yaml_ng_to_json(&v));
        for f in [&FLAVORS[0], &FLAVORS[4]] {
            let saphyr = f.tree(&doc.text);
            let verdict = match (&today, &saphyr) {
                (Ok(a), Ok(b)) if a == b => "identical".to_owned(),
                (Ok(a), Ok(b)) => format!("DIFFERS: first difference at {}", first_difference(a, b, "")),
                (a, b) => format!("today {:?} / saphyr {:?}", a.as_ref().err().map(|e| e.to_string()), b.as_ref().err()),
            };
            println!("    {} [{}]: {verdict}", doc.source, f.name);
        }
    }
}

fn first_difference(a: &J, b: &J, at: &str) -> String {
    match (a, b) {
        (J::Object(x), J::Object(y)) => {
            let xk: Vec<&String> = x.keys().collect();
            let yk: Vec<&String> = y.keys().collect();
            if xk != yk {
                return format!("{at}: keys {xk:?} vs {yk:?}");
            }
            for (k, v) in x {
                if v != &y[k] {
                    return first_difference(v, &y[k], &format!("{at}.{k}"));
                }
            }
            format!("{at}: (order)")
        }
        (J::Array(x), J::Array(y)) if x.len() == y.len() => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                if v != w {
                    return first_difference(v, w, &format!("{at}[{i}]"));
                }
            }
            at.to_owned()
        }
        _ => format!("{at}: {} vs {}", clip(&a.to_string(), 200), clip(&b.to_string(), 200)),
    }
}

// -------------------------------------------------------------------- timing

fn section_timing(docs: &[Doc], iterations: usize) {
    println!("\n== parse time (release build, {iterations} iterations, median of 5 runs, µs per parse)");
    let real: Vec<&Doc> = docs.iter().filter(|d| d.class() != "synthetic").collect();
    let largest_any = real.iter().max_by_key(|d| d.text.len()).unwrap();
    let largest_rules = real.iter().filter(|d| d.kind == Kind::Rules).max_by_key(|d| d.text.len()).unwrap();
    let time = |f: &dyn Fn()| {
        let mut runs: Vec<f64> = (0..5)
            .map(|_| {
                let start = Instant::now();
                for _ in 0..iterations {
                    f();
                }
                start.elapsed().as_secs_f64() * 1e6 / iterations as f64
            })
            .collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        runs[2]
    };
    use saphyr::LoadableYamlNode;
    for doc in [largest_any, largest_rules] {
        let text = &doc.text;
        println!("  {} ({} bytes, {} lines, kind {})", doc.source, text.len(), text.lines().count(), doc.kind.name());
        println!("    serde_yaml_ng::from_str::<Value>                {:>9.1}", time(&|| { std::hint::black_box(serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text).unwrap()); }));
        println!("    serde_saphyr::from_str::<serde_json::Value>     {:>9.1}", time(&|| { std::hint::black_box(serde_saphyr::from_str::<J>(text).unwrap()); }));
        println!("    saphyr::Yaml::load_from_str                     {:>9.1}", time(&|| { std::hint::black_box(saphyr::Yaml::load_from_str(text).unwrap()); }));
        if doc.kind == Kind::Rules {
            let path = Path::new("/rules.yml");
            println!("    today's policy::parse (whole reader)            {:>9.1}", time(&|| { std::hint::black_box(onevcs::spike::rules_parse(path, text).unwrap()); }));
            println!("    port::rules [{}] {:>9.1}", FLAVORS[5].name, time(&|| { std::hint::black_box(port::rules(path, text, &FLAVORS[5]).unwrap()); }));
            println!("    port::rules [{}] {:>9.1}", FLAVORS[8].name, time(&|| { std::hint::black_box(port::rules(path, text, &FLAVORS[8]).unwrap()); }));
        }
    }
}
