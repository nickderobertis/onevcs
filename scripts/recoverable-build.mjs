// The producer and reader hash the same checked input manifest, without Git or a build.
import { readFileSync, readdirSync, lstatSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
export const repositoryRoot = fileURLToPath(new URL("../", import.meta.url));
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
export function inputFiles(root = repositoryRoot) {
  const manifest = JSON.parse(readFileSync(`${root}/scripts/recoverable-build-inputs.json`));
  if (manifest.version !== 1) throw new Error("unsupported build-input manifest");
  const files = new Set(manifest.files);
  const walk = path => {
    const stat = lstatSync(`${root}/${path}`);
    if (stat.isDirectory()) for (const entry of readdirSync(`${root}/${path}`)) walk(`${path}/${entry}`);
    else if (stat.isFile()) files.add(path);
    else throw new Error(`unsupported build input ${path}`);
  };
  for (const directory of manifest.directories) walk(directory);
  for (const {directory,prefix} of manifest.prefixes) {
    for (const name of readdirSync(`${root}/${directory}`)) if (name.startsWith(prefix)) walk(`${directory}/${name}`);
  }
  return [...files].sort();
}
export function sourceFingerprint(root = repositoryRoot) {
  return sha(JSON.stringify(inputFiles(root).map(path => [path,sha(readFileSync(`${root}/${path}`))])));
}
