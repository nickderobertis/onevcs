// The producer and reader hash the same checked input manifest, without Git or a build.
import { readFileSync, readdirSync, lstatSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
export const repositoryRoot = fileURLToPath(new URL("../", import.meta.url));
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
export function inputFiles(root = repositoryRoot) {
  const manifest = JSON.parse(readFileSync(`${root}/scripts/recoverable-build-inputs.json`));
  if (manifest?.version !== 1) throw new Error("unsupported build-input manifest version");
  const relative = (field, path) => {
    if (typeof path !== "string" || path === "" || path.startsWith("/") || path.includes("\\") ||
        path.split("/").some(part => part === "" || part === "." || part === "..")) {
      throw new Error(`build-input manifest ${field} entry ${JSON.stringify(path)} is not a repository-relative path`);
    }
    return path;
  };
  const list = field => {
    if (!Array.isArray(manifest[field])) throw new Error(`build-input manifest ${field} is not a list`);
    return manifest[field];
  };
  const files = new Set(list("files").map(path => relative("files", path)));
  const directories = list("directories").map(path => relative("directories", path));
  const prefixes = list("prefixes").map(entry => {
    if (typeof entry?.prefix !== "string" || entry.prefix === "" || entry.prefix.includes("/")) {
      throw new Error(`build-input manifest prefixes entry ${JSON.stringify(entry)} needs a non-empty prefix without '/'`);
    }
    return { directory: relative("prefixes", entry.directory), prefix: entry.prefix };
  });
  const walk = path => {
    const stat = lstatSync(`${root}/${path}`);
    if (stat.isDirectory()) for (const entry of readdirSync(`${root}/${path}`)) walk(`${path}/${entry}`);
    else if (stat.isFile()) files.add(path);
    else throw new Error(`unsupported build input ${path}`);
  };
  for (const directory of directories) walk(directory);
  for (const {directory,prefix} of prefixes) {
    for (const name of readdirSync(`${root}/${directory}`)) if (name.startsWith(prefix)) walk(`${directory}/${name}`);
  }
  return [...files].sort();
}
export function sourceFingerprint(root = repositoryRoot) {
  return sha(JSON.stringify(inputFiles(root).map(path => [path,sha(readFileSync(`${root}/${path}`))])));
}
