// A cargo runner for wasm32-wasip1: `cargo bench --target wasm32-wasip1`
// runs the benchmark under node's WASI. Arguments after the module
// path, and the environment, pass through to the program.
const { WASI } = require("node:wasi");
const fs = require("node:fs");

const [wasmPath, ...args] = process.argv.slice(2);
const wasi = new WASI({
  version: "preview1",
  args: [wasmPath, ...args],
  env: process.env,
  preopens: { ".": ".", "/": "/" },
  returnOnExit: true,
});
const instance = new WebAssembly.Instance(
  new WebAssembly.Module(fs.readFileSync(wasmPath)),
  wasi.getImportObject(),
);
process.exitCode = wasi.start(instance);
