#!/usr/bin/env node
// A cargo runner for wasm32-wasip1: `cargo bench --target wasm32-wasip1`
// runs the benchmark under node's WASI. Arguments after the module
// path, and the environment, pass through to the program.
//
// It is the runner itself, rather than an argument to `node`, because cargo
// finds a runner whose path has a slash relative to the directory holding
// `.cargo`, whatever directory it runs the program in.

// WASI prints an ExperimentalWarning, which is not the program's output.
process.removeAllListeners("warning");

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
