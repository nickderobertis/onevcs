// onebudgetspec performs every numeric comparison; this adapter enforces its verdict.
import { readFileSync } from "node:fs";
try {
    const report = JSON.parse(readFileSync(0, "utf8"));
    if (report === null || typeof report !== "object" || Array.isArray(report))
        throw new Error("checker report must be an object");
    const results = report.results;
    if (!Array.isArray(results) || !results.length)
        throw new Error("checker returned no budget results");
    for (const result of results) {
        if (
            result === null ||
            typeof result !== "object" ||
            Array.isArray(result) ||
            typeof result.id !== "string" ||
            !result.id ||
            !["within", "over", "error"].includes(result.verdict) ||
            (result.error !== null && typeof result.error !== "string")
        )
            throw new Error(
                "checker result needs string id/verdict and nullable string error",
            );
        if (result.verdict !== "within")
            throw new Error(
                `${result.id}: ${result.verdict}; ${result.error || "inspect the onebudgetspec result"}`,
            );
        console.log(`${result.id}: within budget`);
    }
} catch (error) {
    console.error(
        `Mac budgets: ${error.message}\nACTION: regenerate workflow telemetry and rerun the scheduling checker`,
    );
    process.exitCode = 1;
}
