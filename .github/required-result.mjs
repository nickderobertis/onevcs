// GitHub job results are the only doubled boundary in the subprocess journeys.
const [lane, json] = process.argv.slice(2);
try {
    if (!["cross", "install"].includes(lane) || process.argv.length !== 4)
        throw new Error("expected cross|install and a needs JSON object");
    const needs = JSON.parse(json);
    const object = (value) =>
        value !== null && typeof value === "object" && !Array.isArray(value);
    if (
        !object(needs) ||
        !object(needs.changes) ||
        !object(needs.changes.outputs)
    )
        throw new Error("needs must contain a changes.outputs object");
    if (needs.changes?.result !== "success")
        throw new Error("classification failed/cancelled/missing");
    const outputs = needs.changes.outputs;
    const required =
        outputs.rust === "true" && outputs.macos_required === "true";
    if (
        !["true", "false"].includes(outputs.rust) ||
        !["true", "false"].includes(outputs.macos_required)
    ) {
        throw new Error("missing classification outputs");
    }
    if (outputs.rust === "true" && needs.gate?.result !== "success")
        throw new Error(`${lane}: gate failed/cancelled/missing`);
    if (required) {
        if (needs["macos-pr"]?.result !== "success")
            throw new Error(
                `${lane}: required Mac proof failed/cancelled/missing`,
            );
        console.log(`${lane}: Mac assertions passed`);
    } else {
        if (needs["macos-pr"]?.result !== "skipped")
            throw new Error("omitted Mac lane has an unexpected outcome");
        const reason =
            outputs.rust === "false"
                ? "Rust path filter reports unaffected"
                : "Mac classifier authorizes omission";
        console.log(`${lane}: Mac assertions omitted: ${reason}`);
    }
} catch (error) {
    console.error(
        `${error.message}\nACTION: inspect the prerequisite job results and rerun the required proof`,
    );
    process.exitCode = 1;
}
