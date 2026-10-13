import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

// Only release-plz's same-repository branch plus a semantic diff authorizes omission.
// The PAT author is also the human maintainer, so login/title confer no authority.
export function classify(
    event,
    base,
    head,
    eventName = process.env.GITHUB_EVENT_NAME,
) {
    const pr = event?.pull_request;
    const name = eventName || (pr ? "pull_request" : event?.event_name);
    const answer = (isReleaseOnly, macosRequired, reason) => ({
        isReleaseOnly,
        macosRequired,
        reason,
    });
    if (name && name !== "pull_request")
        return answer(false, false, "non-PR event");
    if (!pr) return answer(false, true, "unknown PR input requires coverage");
    if (
        typeof event.repository?.full_name !== "string" ||
        !event.repository.full_name ||
        typeof pr.head?.ref !== "string" ||
        pr.head?.repo?.full_name !== event.repository.full_name ||
        !/^release-plz-/.test(pr.head?.ref || "")
    )
        return answer(false, true, "ordinary or fork PR");
    try {
        const git = (...args) =>
            execFileSync("git", args, {
                encoding: "utf8",
                stdio: ["pipe", "pipe", "pipe"],
                maxBuffer: 20 * 1024 * 1024,
            });
        // Resolve refs before constructing object:path arguments, reject option-like refs.
        for (const ref of [base, head])
            if (!ref || ref.startsWith("-"))
                throw new Error("missing/invalid ref");
        const b = git("rev-parse", "--verify", `${base}^{commit}`).trim();
        const h = git("rev-parse", "--verify", `${head}^{commit}`).trim();
        const paths = git("diff", "--name-only", "-z", b, h)
            .split("\0")
            .filter(Boolean);
        if (!paths.length) throw new Error("empty diff");
        for (const path of paths) {
            if (
                ["CHANGELOG.md", "crates/onevcs-testing/CHANGELOG.md"].includes(
                    path,
                )
            ) {
                git("show", `${b}:${path}`);
                git("show", `${h}:${path}`);
                continue;
            }
            if (
                ![
                    "crates/onevcs/Cargo.toml",
                    "crates/onevcs-testing/Cargo.toml",
                    "Cargo.lock",
                    "compat/Cargo.lock",
                ].includes(path)
            ) {
                throw new Error(`non-release path ${path}`);
            }
            const before = git("show", `${b}:${path}`);
            const after = git("show", `${h}:${path}`);
            const result = execFileSync(
                "python3",
                [
                    fileURLToPath(
                        new URL("./version-only.py", import.meta.url),
                    ),
                    path,
                ],
                {
                    input: JSON.stringify({ before, after }),
                    encoding: "utf8",
                },
            );
            if (result.trim() !== "true")
                throw new Error(`non-version content ${path}`);
        }
        return answer(
            true,
            false,
            "same-repository release-plz branch; semantic version/changelog-only diff",
        );
    } catch (error) {
        return answer(
            false,
            true,
            `unverified release diff requires coverage: ${error.message}`,
        );
    }
}

if (
    process.argv[1]?.endsWith("/macos-policy.mjs") ||
    process.argv[1] === ".github/macos-policy.mjs"
) {
    const [command, ...args] = process.argv.slice(2);
    if (
        command !== "classify" ||
        args.length !== 6 ||
        args[0] !== "--event" ||
        args[2] !== "--base" ||
        args[4] !== "--head"
    ) {
        console.error("usage: classify --event JSON --base REF --head REF");
        process.exitCode = 1;
    } else {
        let event;
        try {
            event = JSON.parse(readFileSync(args[1], "utf8"));
        } catch {
            event = null;
        }
        console.log(JSON.stringify(classify(event, args[3], args[5])));
    }
}
