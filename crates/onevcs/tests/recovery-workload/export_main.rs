//! Optimized in-process export budgets, outside the recovery read's timed figure.
#![cfg(unix)]

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use flate2::{write::GzEncoder, Compression};
use onevcs::{ExportRequest, Providers, TermScope};
use onevcs_testing::{boundary, export_fixtures};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const PATHS: usize = 496;
const BYTES: usize = 194_900_000;
const LARGEST: usize = 37_100_000;
const LFS_OBJECTS: usize = 462;

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
    std::fs::write(path, bytes).expect("file");
}

/// A stable seed supplies camera entropy and coordinates, never private terms.
struct Seed(u64);
impl Seed {
    fn byte(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as u8
    }
}

fn segment(bytes: &mut Vec<u8>, marker: u8, data: &[u8]) {
    bytes.extend_from_slice(&[255, marker]);
    bytes.extend_from_slice(
        &u16::try_from(data.len() + 2)
            .expect("JPEG segment")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(data);
}

/// A valid 1280x720 grayscale JPEG, with EXIF/XMP from the shared carrier and
/// independently seeded DCT signs. COM padding fixes the workload's raw size.
fn frame(seed: &mut Seed, size: usize, sample: &[u8]) -> Vec<u8> {
    let mut bytes = vec![255, 216];
    let mut at = 2;
    while sample[at..at + 2] == [255, 225] {
        let len = usize::from(u16::from_be_bytes([sample[at + 2], sample[at + 3]]));
        bytes.extend_from_slice(&sample[at..at + 2 + len]);
        at += 2 + len;
    }
    let mut quantization = vec![0];
    quantization.extend_from_slice(&[16; 64]);
    segment(&mut bytes, 219, &quantization);
    segment(&mut bytes, 192, &[8, 2, 208, 5, 0, 1, 1, 17, 0]);
    for table in [0, 16] {
        let mut huffman = vec![table, 1];
        huffman.extend_from_slice(&[0; 15]);
        huffman.push(1); // category one, code zero; one sign bit per coefficient
        segment(&mut bytes, 196, &huffman);
    }
    // 14,400 blocks, 64 coefficients, two bits each; stuff entropy's FF bytes.
    let mut entropy = Vec::with_capacity(240_000);
    for _ in 0..14_400 * 16 {
        let byte = seed.byte() & 0x55;
        entropy.push(byte);
    }
    let scan_len = 10 + entropy.len() + 2;
    while bytes.len() + scan_len < size {
        let remaining = size - bytes.len() - scan_len;
        assert!(remaining >= 4, "JPEG padding segment");
        let mut len = (remaining - 4).min(65_530);
        if (1..4).contains(&(remaining - len - 4)) {
            len -= 4;
        }
        let padding: Vec<u8> = (0..len).map(|_| seed.byte()).collect();
        segment(&mut bytes, 254, &padding);
    }
    segment(&mut bytes, 218, &[1, 1, 0, 0, 63, 0]);
    bytes.extend_from_slice(&entropy);
    bytes.extend_from_slice(&[255, 217]);
    assert_eq!(bytes.len(), size);
    bytes
}

fn stl(seed: &mut Seed, triangles: usize) -> Vec<u8> {
    let mut bytes = vec![0; 80];
    bytes[..21].copy_from_slice(b"Generated print model");
    bytes.extend_from_slice(&(triangles as u32).to_le_bytes());
    for _ in 0..triangles {
        for _ in 0..12 {
            bytes.extend_from_slice(&(f32::from(seed.byte()) / 8.).to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes
}

fn realistic_set() -> Vec<(String, Vec<u8>)> {
    let samples = export_fixtures::sample_set(None);
    let sample = |name: &str| {
        samples
            .iter()
            .find(|(path, _)| path == name)
            .unwrap()
            .1
            .as_slice()
    };
    let mut seed = Seed(0x83a7_19ef_6b05_c24d);
    let mut files = vec![(
        ".gitattributes".into(),
        export_fixtures::lfs_attributes().into_bytes(),
    )];
    for n in 0..7 {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::none());
        for step in 0..28_000 {
            writeln!(
                encoder,
                "G1 X{}.{} Y{}.{} Z{} E{}.{} F1800",
                seed.byte(),
                seed.byte(),
                seed.byte(),
                seed.byte(),
                step / 500,
                seed.byte(),
                seed.byte()
            )
            .unwrap();
        }
        files.push((format!("print-{n}.gcode.gz"), encoder.finish().unwrap()));
    }
    for n in 0..6 {
        let triangles = if n == 0 {
            (LARGEST - 84) / 50
        } else {
            40_000 + n * 100
        };
        files.push((format!("model-{n}.stl"), stl(&mut seed, triangles)));
    }
    for n in 0..3 {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file("[Content_Types].xml", options).unwrap();
        writer.write_all(b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/></Types>\n").unwrap();
        writer.start_file("3D/3dmodel.model", options).unwrap();
        writer
            .write_all(b"<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" unit=\"millimeter\"><resources><object id=\"1\" type=\"model\"><mesh><vertices>\n")
            .unwrap();
        for _ in 0..20_000 {
            writeln!(
                writer,
                "<vertex x=\"{}\" y=\"{}\" z=\"{}\"/>",
                seed.byte(),
                seed.byte(),
                seed.byte()
            )
            .unwrap();
        }
        writer
            .write_all(b"</vertices><triangles><triangle v1=\"0\" v2=\"1\" v3=\"2\"/></triangles></mesh></object></resources><build><item objectid=\"1\"/></build></model>\n")
            .unwrap();
        writer
            .start_file("Metadata/thumbnail.png", options)
            .unwrap();
        writer.write_all(sample("frame.png")).unwrap();
        files.push((
            format!("model-{n}.3mf"),
            writer.finish().unwrap().into_inner(),
        ));
    }
    for n in 0..28 {
        files.push((
            format!("measure-{n}.json"),
            json!({"frame": n, "temperature": 205, "layer": n * 20})
                .to_string()
                .into_bytes(),
        ));
    }
    for n in 0..4 {
        files.push((
            format!("notes-{n}.md"),
            format!("# Print {n}\nNeutral camera and slicer evidence.\n").into_bytes(),
        ));
    }
    files.push((
        "analyze.py".into(),
        b"import json\nprint(json.dumps({'status': 'complete'}))\n".to_vec(),
    ));
    let remaining = BYTES - files.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
    for n in 0..446 {
        let size = remaining / 446 + usize::from(n < remaining % 446);
        files.push((
            format!("frame-{n:03}.jpg"),
            frame(&mut seed, size, sample("frame.jpg")),
        ));
    }
    assert_eq!(files.len(), PATHS);
    assert_eq!(files.iter().map(|(_, b)| b.len()).sum::<usize>(), BYTES);
    // Binary STL sizes must be 84 plus a whole number of 50-byte triangles.
    assert_eq!(files.iter().map(|(_, b)| b.len()).max(), Some(LARGEST - 16));
    files
}

fn object(store: &Path, oid: &str) -> PathBuf {
    store
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
}

fn pointer(bytes: &[u8]) -> Vec<u8> {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        digest(bytes),
        bytes.len()
    )
    .into_bytes()
}

fn tracked(path: &str) -> bool {
    [".jpg", ".stl", ".3mf", ".gz"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

fn endpoint(repo: &Path, store: &Path) {
    git(
        repo,
        &["config", "lfs.url", &format!("file://{}", store.display())],
    );
}

fn verify(workload: &boundary::Workload, head: &str, files: &[(String, Vec<u8>)], store: &Path) {
    let tree = git(
        &workload.destination,
        &["ls-tree", "-r", "--name-only", head, "evidence"],
    );
    assert_eq!(String::from_utf8(tree).unwrap().lines().count(), PATHS);
    for (path, bytes) in files {
        let blob = git(
            &workload.destination,
            &["show", &format!("{head}:evidence/{path}")],
        );
        if tracked(path) {
            assert_eq!(blob, pointer(bytes), "{path}");
            let oid = digest(bytes);
            assert_eq!(
                std::fs::read(object(store, &oid)).expect("uploaded object"),
                *bytes,
                "{path}"
            );
            assert_eq!(
                std::fs::read(object(&workload.destination.join(".git/lfs"), &oid))
                    .expect("local object"),
                *bytes,
                "{path}"
            );
        } else {
            assert_eq!(blob, *bytes, "{path}");
        }
    }
}

fn store_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    fn visit(root: &Path, at: &Path, files: &mut std::collections::BTreeMap<PathBuf, String>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    digest(&std::fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn fingerprint(root: &Path) -> String {
    let output = Command::new("node").current_dir(root).args(["--input-type=module", "-e", "import {sourceFingerprint} from './scripts/recoverable-build.mjs'; process.stdout.write(sourceFingerprint());"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn diagnostics(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .rev()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| line["check"] == "export")
        .expect("export diagnostics")
}

/// Completed downloads in git-lfs's real progress format:
/// direction current/total bytes/size name. Intermediate updates count no bytes.
fn progress(path: &Path, trace_path: &Path, files: &[(String, Vec<u8>)]) -> Value {
    let text = std::fs::read_to_string(path).expect("git-lfs transfer progress file");
    let known: Vec<_> = files
        .iter()
        .filter(|(path, _)| tracked(path))
        .map(|(path, bytes)| (path, digest(bytes), bytes.len() as u64))
        .collect();
    let mut transfers = std::collections::BTreeSet::new();
    let mut completed = std::collections::BTreeMap::<String, u64>::new();
    for line in text.lines() {
        let mut fields = line.splitn(4, ' ');
        if fields.next() != Some("download") {
            continue;
        }
        let position = fields.next().expect("position");
        let (current, total) = fields
            .next()
            .expect("bytes")
            .split_once('/')
            .expect("byte count");
        let (current, total): (u64, u64) = (current.parse().unwrap(), total.parse().unwrap());
        let name = fields.next().expect("object path");
        if current == total && transfers.insert((position.to_owned(), name.to_owned())) {
            let matched: Vec<_> = known
                .iter()
                .filter(|(path, oid, _)| {
                    name == **path || name.ends_with(&format!("/{path}")) || name.contains(oid)
                })
                .collect();
            assert_eq!(matched.len(), 1, "progress object {name}");
            assert_eq!(total, matched[0].2);
            *completed.entry(matched[0].1.clone()).or_default() += total;
        }
    }
    let trace = std::fs::read_to_string(trace_path).expect("git-lfs client trace");
    assert!(
        trace.contains("trace git-lfs:"),
        "a real git-lfs client wrote the trace"
    );
    let mut completion_events = transfers.len();
    let counted_from = if text.is_empty() {
        // The standalone-file adapter does not send byte progress. Count only the
        // real client's successful response to a preceding download job, never a
        // fetch request or a queued job. Upload completions cannot satisfy this.
        let sizes: std::collections::BTreeMap<_, _> = known
            .iter()
            .map(|(_, oid, size)| (oid.as_str(), *size))
            .collect();
        let mut pending = std::collections::BTreeSet::new();
        for line in trace.lines() {
            if let Some((_, message)) = line.split_once("sending message: ") {
                let message: Value =
                    serde_json::from_str(message).expect("client transfer request JSON");
                if message["event"] == "download" {
                    let oid = message["oid"].as_str().expect("download oid");
                    assert_eq!(
                        message["size"].as_u64(),
                        sizes.get(oid).copied(),
                        "source object size"
                    );
                    pending.insert(oid.to_owned());
                }
            } else if let Some((_, message)) = line.split_once("received response: ") {
                let message: Value =
                    serde_json::from_str(message).expect("client transfer response JSON");
                if message["event"] == "complete" {
                    let oid = message["oid"].as_str().expect("completed oid");
                    if pending.remove(oid) && message.get("error").is_none() {
                        assert!(
                            message["path"].as_str().is_some(),
                            "download response has its materialized path"
                        );
                        *completed.entry(oid.to_owned()).or_default() += sizes[oid];
                        completion_events += 1;
                    }
                }
            }
        }
        "git-lfs-trace-complete"
    } else {
        "git-lfs-progress"
    };
    json!({"objects": completed.len(), "bytes": completed.values().sum::<u64>(), "progress_lines": text.lines().count(), "trace_lines":trace.lines().count(), "counted_from":counted_from, "completion_events":completion_events, "completed": completed})
}

fn workload_lock() -> std::fs::File {
    use fs4::fs_std::FileExt;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/recovery-workload.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    lock
}

fn require_release_profile() {
    #[cfg(debug_assertions)]
    panic!("export budget journey must run under release profile");
}

#[test]
fn realistic_export_telemetry() {
    export_scenario(false);
}

#[test]
fn realistic_export_source_lfs_telemetry() {
    export_scenario(true);
}

fn export_scenario(downloads: bool) {
    require_release_profile();
    if !Command::new("git-lfs")
        .arg("version")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("skipped: git-lfs is unavailable");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/budget-records");
    std::fs::create_dir_all(&directory).unwrap();
    let _lock = workload_lock();
    let record_name = if downloads {
        "export-source-lfs.json"
    } else {
        "export.json"
    };
    let _ = std::fs::remove_file(directory.join(record_name));
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let holder = tempfile::Builder::new()
        .prefix("export-workload-")
        .tempdir_in(scratch)
        .unwrap();
    let workload = boundary::build(&holder.path().join("host")).unwrap();
    // The workload lock serializes environment changes before export starts its workers.
    std::env::set_var("HOME", &workload.root);
    std::env::set_var("ONEVCS_HOME", &workload.home);
    std::env::remove_var("GIT_CONFIG_GLOBAL");
    std::env::remove_var("GIT_LFS_PROGRESS");
    std::env::remove_var("GIT_TRACE");
    std::env::remove_var("GIT_TRANSFER_TRACE");
    let diagnostic_path = holder.path().join("diagnostics.ndjson");
    std::env::set_var("ONEVCS_BOUNDARY_DIAGNOSTICS", &diagnostic_path);
    let source = workload.root.join("checkouts/quietvault00");
    git(&source, &["checkout", "-q", "-b", "print-evidence", "main"]);
    let files = realistic_set();
    for (path, bytes) in &files {
        write(&source.join("examples").join(path), bytes);
    }
    // Plain blobs in the first scenario, even though the exported attributes track them.
    git(
        &source,
        &[
            "-c",
            "filter.lfs.clean=cat",
            "-c",
            "filter.lfs.required=false",
            "add",
            "examples",
        ],
    );
    git(&source, &["commit", "-q", "-m", "docs: print evidence"]);
    let destination_store = workload.destination_origin.join("lfs");
    std::fs::create_dir_all(&destination_store).unwrap();
    endpoint(&workload.destination, &workload.destination_origin);
    write(
        &workload.destination.join(".gitattributes"),
        export_fixtures::lfs_attributes().as_bytes(),
    );
    git(&workload.destination, &["add", ".gitattributes"]);
    git(
        &workload.destination,
        &["commit", "-q", "-m", "chore: track print evidence"],
    );
    git(&workload.destination, &["push", "-q", "origin", "main"]);

    let request = ExportRequest {
        from: workload.source.clone(),
        branch: "print-evidence".into(),
        directory: "examples".into(),
        to: boundary::DESTINATION.into(),
        target_directory: "evidence".into(),
        branch_name: "print-output".into(),
        term_scope: TermScope::default(),
        accepted_opaque: vec!["*.jpg".into(), "*.stl".into(), "*.3mf".into()],
    };
    let source_hash = fingerprint(&root);
    let producer = std::env::current_exe().unwrap();
    let producer_hash = digest(&std::fs::read(&producer).unwrap());
    std::fs::copy(&producer, directory.join("export-workload")).unwrap();
    let binary = std::env::var_os("ONEVCS_RECOVERY_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/release/onevcs"));
    let binary_hash = digest(&std::fs::read(&binary).unwrap());
    static LOCAL_RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let run_id = std::env::var("ONEVCS_RECOVERY_INVOCATION").unwrap_or_else(|_| {
        LOCAL_RUN
            .get_or_init(|| {
                digest(format!("{} {:?}", std::process::id(), Instant::now()).as_bytes())
            })
            .clone()
    });
    let build = json!({"run_id":run_id,"binary":"onevcs","binary_sha256":binary_hash,"source_sha256":source_hash});
    let load1: f64 = std::fs::read_to_string("/proc/loadavg")
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap();
    let covered = json!({"identities":20,"terms":2000,"paths":PATHS,"bytes":BYTES,"largest":LARGEST-16,"lfs_objects":LFS_OBJECTS,"lfs_bytes":194_898_393,"patterns":3});
    let finish = |record: &Value| {
        assert_eq!(source_hash, fingerprint(&root), "inputs remained unchanged");
        write(
            &directory.join(record_name),
            &serde_json::to_vec_pretty(record).unwrap(),
        );
        if std::env::var_os("ONEVCS_RECOVERY_INVOCATION").is_none() {
            std::fs::copy(&binary, directory.join("onevcs")).unwrap();
            let mut invocation = build.clone();
            invocation["state"] = json!("complete");
            write(
                &directory.join("recoverable-invocation.json"),
                &serde_json::to_vec(&invocation).unwrap(),
            );
        }
    };
    if !downloads {
        let started = Instant::now();
        let exported =
            onevcs::export(&Providers::real(), &request).expect("realistic export lands");
        let wall_us = started.elapsed().as_micros() as u64;
        let phases = diagnostics(&diagnostic_path);
        eprintln!("realistic export {wall_us}us: {phases}");
        verify(&workload, &exported.head, &files, &destination_store);
        // SAFETY: rusage is plain data; getrusage initializes the supplied valid buffer
        // for this process before any of its fields are read.
        let usage: libc::rusage = unsafe {
            let mut usage = std::mem::zeroed();
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut usage), 0);
            usage
        };
        let peak_rss_kib = u64::try_from(usage.ru_maxrss).unwrap();
        let peak_rss_kib = if cfg!(target_os = "macos") {
            peak_rss_kib / 1024
        } else {
            peak_rss_kib
        };
        let record = json!({"version":3,"build":build,"producer_sha256":producer_hash,"scenario":"realistic-export","covered":covered,"wall_us":wall_us,"phases":phases,"peak_rss_kib":peak_rss_kib,"load1":load1});

        finish(&record);
        return;
    }

    // Commit the exact same content as pointers, with objects only at source origin.
    let source_origin = workload.root.join("origins/quietvault00.git");
    let source_store = source_origin.join("lfs");
    std::fs::create_dir_all(&source_store).unwrap();
    endpoint(&source, &source_origin);
    for (path, bytes) in &files {
        if tracked(path) {
            write(&object(&source_store, &digest(bytes)), bytes);
            write(&source.join("examples").join(path), &pointer(bytes));
        }
    }
    git(
        &source,
        &[
            "-c",
            "filter.lfs.clean=cat",
            "-c",
            "filter.lfs.required=false",
            "add",
            "examples",
        ],
    );
    git(
        &source,
        &["commit", "-q", "-m", "docs: preserve print objects"],
    );
    let _ = std::fs::remove_dir_all(source.join(".git/lfs"));
    let blocked = holder.path().join("unwritable-endpoint");
    git(
        &workload.root,
        &["init", "--bare", "-q", &blocked.to_string_lossy()],
    );
    std::fs::write(blocked.join("lfs"), "a file cannot hold an LFS store").unwrap();
    endpoint(&workload.destination, &blocked);
    let mut retry = request.clone();
    retry.branch_name = "print-retry".into();
    let source_before = store_snapshot(&source_store);
    let first_progress = holder.path().join("first-progress");
    std::fs::write(&first_progress, "").unwrap();
    let first_trace = holder.path().join("first-trace");
    std::fs::write(&first_trace, "").unwrap();
    std::env::set_var("GIT_TRACE", &first_trace);
    std::env::set_var("GIT_TRANSFER_TRACE", "1");
    std::env::set_var("GIT_LFS_PROGRESS", &first_progress);
    let refusal = onevcs::export(&Providers::real(), &retry).expect_err("upload refuses");
    assert!(
        refusal
            .to_string()
            .contains("its destination's Git LFS store could not take its objects"),
        "{refusal}"
    );
    let actual_scope = |mut counts: Value| {
        let phases = diagnostics(&diagnostic_path);
        for key in ["identities", "terms"] {
            assert_eq!(phases[key], covered[key], "actual source export scope");
            counts[key] = phases[key].clone();
        }
        counts
    };
    let first = actual_scope(progress(&first_progress, &first_trace, &files));
    assert_eq!(first["objects"], LFS_OBJECTS);
    endpoint(&workload.destination, &workload.destination_origin);
    let second_progress = holder.path().join("second-progress");
    std::fs::write(&second_progress, "").unwrap();
    let second_trace = holder.path().join("second-trace");
    std::fs::write(&second_trace, "").unwrap();
    std::env::set_var("GIT_TRACE", &second_trace);
    std::env::set_var("GIT_LFS_PROGRESS", &second_progress);
    let landed = onevcs::export(&Providers::real(), &retry).expect("cached retry lands");
    let second = actual_scope(progress(&second_progress, &second_trace, &files));
    assert_eq!(second["bytes"], 0, "retry downloads nothing");
    verify(&workload, &landed.head, &files, &destination_store);
    for (path, bytes) in &files {
        if tracked(path) {
            assert_eq!(
                std::fs::read(object(&source.join(".git/lfs"), &digest(bytes))).unwrap(),
                *bytes
            );
        }
    }
    assert_eq!(
        source_before,
        store_snapshot(&source_store),
        "download does not alter the source origin store"
    );
    assert_eq!(source_hash, fingerprint(&root), "inputs remained unchanged");
    let object_sizes: std::collections::BTreeMap<_, _> = files
        .iter()
        .filter(|(path, _)| tracked(path))
        .map(|(_, bytes)| (digest(bytes), bytes.len() as u64))
        .collect();
    let download = json!({"version":3,"build":build,"producer_sha256":producer_hash,"scenario":"realistic-export-source-lfs","covered":covered,"attempts":[first,second],"object_sizes":object_sizes,"load1":load1});
    finish(&download);
    eprintln!(
        "source LFS cold {} bytes; retry {} bytes; load1 {load1}",
        download["attempts"][0]["bytes"], download["attempts"][1]["bytes"]
    );
}

#[test]
fn source_trace_uses_successful_download_completions() {
    if !Command::new("git-lfs")
        .arg("version")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("skipped: git-lfs is unavailable");
        return;
    }
    let _lock = workload_lock();
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let holder = tempfile::Builder::new()
        .prefix("export-transfer-proof-")
        .tempdir_in(scratch)
        .unwrap();
    let repo = holder.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["lfs", "install", "--local"]);
    let origin = holder.path().join("origin.git");
    git(&repo, &["init", "--bare", "-q", &origin.to_string_lossy()]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    endpoint(&repo, &origin);
    let files: Vec<_> = export_fixtures::sample_set(None)
        .into_iter()
        .filter(|(path, _)| path == "frame.jpg")
        .collect();
    write(
        &repo.join(".gitattributes"),
        b"*.jpg filter=lfs diff=lfs merge=lfs -text\n",
    );
    write(&repo.join("frame.jpg"), &files[0].1);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "fixture"]);
    let oid = digest(&files[0].1);
    git(&repo, &["lfs", "push", "--object-id", "origin", &oid]);
    std::fs::remove_dir_all(repo.join(".git/lfs")).unwrap();
    let progress_file = holder.path().join("progress");
    let trace_file = holder.path().join("trace");
    std::fs::write(&progress_file, "").unwrap();
    let transfer = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&repo)
            .args(args)
            .env("GIT_TRACE", &trace_file)
            .env("GIT_TRANSFER_TRACE", "1")
            .env("GIT_LFS_PROGRESS", &progress_file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    transfer(&["lfs", "fetch", "origin", "main"]);
    let measured = progress(&progress_file, &trace_file, &files);
    assert_eq!(measured["objects"], 1);
    assert_eq!(measured["bytes"], files[0].1.len());
    assert_eq!(measured["completion_events"], 1);
    assert_eq!(
        std::fs::read(object(&repo.join(".git/lfs"), &oid)).unwrap(),
        files[0].1
    );

    // Add an actual upload completion to the same client trace: it is not a download.
    let other = holder.path().join("upload.git");
    git(&repo, &["init", "--bare", "-q", &other.to_string_lossy()]);
    endpoint(&repo, &other);
    transfer(&["lfs", "push", "--object-id", "origin", &oid]);
    assert_eq!(
        std::fs::read(object(&other.join("lfs"), &oid)).unwrap(),
        files[0].1
    );
    let both_transfers = std::fs::read_to_string(&trace_file).unwrap();
    assert!(both_transfers.contains("\"event\":\"upload\""));
    assert_eq!(
        both_transfers
            .lines()
            .filter(|line| line.contains("received response: ")
                && line.contains("\"event\":\"complete\""))
            .count(),
        2
    );
    assert_eq!(
        progress(&progress_file, &trace_file, &files)["bytes"],
        files[0].1.len()
    );

    // Induced incomplete trace: real queued jobs and finished-job messages remain,
    // but successful completion responses have been removed. Requests prove no bytes.
    let trace = std::fs::read_to_string(&trace_file).unwrap();
    let requests_only = trace
        .lines()
        .filter(|line| !line.contains("\"event\":\"complete\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(requests_only.contains("\"event\":\"download\""));
    std::fs::write(&trace_file, requests_only).unwrap();
    std::fs::write(&progress_file, "").unwrap();
    let incomplete = progress(&progress_file, &trace_file, &files);
    assert_eq!(incomplete["objects"], 0);
    assert_eq!(incomplete["bytes"], 0);
}
