// Analysis consumes the recorded workload only; thresholds belong to registration.
import { readFileSync } from "node:fs";
import { report } from "@onebudgetspec/sdk";
import { sourceDigest } from "./workflow-graph.mjs";
try {
    const [flag, metric, telemetry, path] = process.argv.slice(2);
    if (
        flag !== "--metric" ||
        telemetry !== "--telemetry" ||
        !["jobs-per-run", "runs-per-change"].includes(metric) ||
        !path
    )
        throw new Error(
            "expected --metric jobs-per-run|runs-per-change --telemetry PATH",
        );
    const record = JSON.parse(readFileSync(path, "utf8"));
    if (
        record.schema_version !== 1 ||
        record.source_digest !== sourceDigest() ||
        record.event_classes.length !== 12 ||
        !record.scenarios.length ||
        !record.lifecycles.length
    )
        throw new Error("missing/stale/incomplete workload telemetry");
    const value = record.metrics[metric.replaceAll("-", "_")];
    const detail =
        metric === "jobs-per-run"
            ? record.scenarios.map((s) => ({
                  event: s.id,
                  diff: s.diff,
                  fork: s.fork,
                  pypi: s.pypi,
                  npm: s.npm,
                  crate: s.crate,
                  jobs: s.rows.filter((r) => r.macos),
              }))
            : record.lifecycles;
    if (
        !Number.isInteger(value) ||
        !report(
            value,
            JSON.stringify({ source_digest: record.source_digest, detail }),
        )
    )
        throw new Error(
            "invoke through onebudgetspec with ONEBUDGETSPEC_RESULT",
        );
} catch (error) {
    console.error(
        `Mac budget: ${error.message}\nACTION: run github-workflows:test, then github-workflows:budgets`,
    );
    process.exitCode = 1;
}
