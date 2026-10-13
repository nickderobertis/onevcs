// onebudgetspec performs every numeric comparison; this adapter enforces its verdict.
import { readFileSync } from "node:fs";
try {
    const report = JSON.parse(readFileSync(0, "utf8"));
    const results = report.results;
    if (!Array.isArray(results) || !results.length)
        throw new Error("checker returned no budget results");
    for (const result of results) {
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
