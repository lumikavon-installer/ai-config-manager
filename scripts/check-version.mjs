import { readFileSync } from "node:fs";

const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const version = json("package.json").version;
const lock = json("package-lock.json");
const versions = [
  ["package-lock.json", lock.version],
  ["package-lock.json root", lock.packages[""].version],
  ["src-tauri/tauri.conf.json", json("src-tauri/tauri.conf.json").version],
];
for (const path of ["Cargo.toml", "crates/core/Cargo.toml", "src-tauri/Cargo.toml"]) {
  versions.push([path, readFileSync(path, "utf8").match(/^version\s*=\s*"([^"]+)"/m)?.[1]]);
}
if (process.env.GITHUB_REF_TYPE === "tag") {
  versions.push(["tag", process.env.GITHUB_REF_NAME?.replace(/^v/, "")]);
}
for (const [path, actual] of versions) {
  if (actual !== version) {
    console.error(`${path}: expected ${version}, got ${actual}`);
    process.exitCode = 1;
  }
}
