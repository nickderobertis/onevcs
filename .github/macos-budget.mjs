// Analysis consumes the recorded workload only; thresholds belong to registration.
import { readFileSync } from "node:fs";
import { report } from "@onebudgetspec/sdk";
import { sourceDigest } from "./workflow-graph.mjs";
try {
    const [metricFlag, metric, telemetryFlag, telemetryPath] =
        process.argv.slice(2);
    if (
        metricFlag !== "--metric" ||
        telemetryFlag !== "--telemetry" ||
        !["jobs-per-run", "runs-per-change"].includes(metric) ||
        !telemetryPath
    )
        throw new Error(
            "expected --metric jobs-per-run|runs-per-change --telemetry PATH",
        );
    const record = JSON.parse(readFileSync(telemetryPath, "utf8"));
    const object = (value) =>
        value !== null && typeof value === "object" && !Array.isArray(value);
    if (
        !object(record) ||
        !Array.isArray(record.event_classes) ||
        !Array.isArray(record.scenarios) ||
        !Array.isArray(record.lifecycles) ||
        !object(record.metrics)
    )
        throw new Error(
            "telemetry needs event_classes/scenarios/lifecycles arrays and a metrics object",
        );
    if (
        !record.scenarios.every((s) => object(s) && Array.isArray(s.rows)) ||
        !record.lifecycles.every(
            (l) =>
                object(l) &&
                Array.isArray(l.spending) &&
                Array.isArray(l.events) &&
                Array.isArray(l.workflow_runs),
        )
    )
        throw new Error(
            "telemetry scenario rows and lifecycle events/runs/spending must be arrays",
        );
    if (
        record.schema_version !== 2 ||
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
        value < 0 ||
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
