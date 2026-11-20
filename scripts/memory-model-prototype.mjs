// Builds the memory-model test programs, runs each on wasmtime and Node, and prints a results table.
// `node scripts/memory-model-prototype.mjs --wasi <file>` runs one module on Node's WASI instead.
import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { platform } from "node:os";

if (process.argv[2] === "--wasi") {
  const { WASI } = await import("node:wasi");
  const wasi = new WASI({ version: "preview1" });
  const module = await WebAssembly.compile(readFileSync(process.argv[3]));
  wasi.start(await WebAssembly.instantiate(module, wasi.getImportObject()));
  process.exit(0);
}

const root = new URL("..", import.meta.url).pathname;
const out = `${root}target/memory-model-prototype`;
execFileSync("cargo", ["run", "-q", "--release", "-p", "check-wasm-proto", "--", out], { stdio: "inherit" });

const runtimes = {
  wasmtime: (file) => ["wasmtime", ["run", "-W", "gc=y", file]],
  node: (file) => [process.execPath, ["--no-warnings", new URL(import.meta.url).pathname, "--wasi", file]],
};

// Peak memory comes from time(1): BSD prints bytes with -l, GNU prints kilobytes with -v.
function run([command, args]) {
  const flag = platform() === "darwin" ? "-l" : "-v";
  const result = spawnSync("/usr/bin/time", [flag, command, ...args], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed:\n${result.stderr}`);
  const values = Object.fromEntries(result.stdout.trim().split("\n").map((line) => line.split(" ")));
  const bsd = result.stderr.match(/(\d+)\s+maximum resident set size/);
  const peak = bsd ? Number(bsd[1]) : Number(result.stderr.match(/Maximum resident set size \(kbytes\): (\d+)/)[1]) * 1024;
  return { ...values, peak };
}

const names = readdirSync(out).filter((f) => f.endsWith(".wasm")).map((f) => f.slice(0, -5)).sort();
for (const name of names) {
  if (!WebAssembly.validate(readFileSync(`${out}/${name}.wasm`))) throw new Error(`${name}.wasm is invalid`);
}

const ms = (ns) => (Number(ns) / 1e6).toFixed(1);
const mb = (bytes) => (bytes / 2 ** 20).toFixed(1);
const rows = [];
const checksums = {};
for (const [runtime, command] of Object.entries(runtimes)) {
  const baseline = run(command(`${out}/empty.wasm`)).peak;
  for (const name of names.filter((n) => n !== "empty")) {
    const r = run(command(`${out}/${name}.wasm`));
    const test = name.split("-")[0];
    checksums[test] ??= r.checksum;
    if (checksums[test] !== r.checksum) throw new Error(`${name} on ${runtime}: checksum ${r.checksum}, expected ${checksums[test]}`);
    const size = statSync(`${out}/${name}.wasm`).size;
    rows.push(`| ${name} | ${runtime} | ${ms(r.total_ns)} | ${ms(r.worst_ns)} | ${mb(r.peak - baseline)} | ${size} |`);
  }
}
console.log("| Program | Runtime | Total ms | Worst unit ms | Peak MB over empty | Module bytes |");
console.log("|---|---|---|---|---|---|");
console.log(rows.join("\n"));
