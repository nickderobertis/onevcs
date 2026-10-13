import { execFileSync } from "node:child_process";
import { readdirSync } from "node:fs";
for (const dir of [".github", ".github/tests"])
    for (const file of readdirSync(dir).filter((f) => f.endsWith(".mjs")))
        execFileSync("node", ["--check", `${dir}/${file}`], {
            stdio: "inherit",
        });
