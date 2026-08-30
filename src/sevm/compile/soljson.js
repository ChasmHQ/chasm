// Drives solc's Emscripten build, for machines no native binary runs on.
//
// soljson.js is a ~9MB JS bundle with the wasm inlined, not a WASI module, so a JS
// runtime is the only way to call it — there is no pure-Python route. It exposes solc's
// C entry point, which the solc-js package wraps and which is stable from 0.6:
// `solidity_compile(input, readCallback, context)`. sevm inlines every source in the
// standard-JSON input, so the import callback is never needed and is passed as null.
//
// Usage: node soljson.js <path to soljson> < input.json > output.json

const soljson = process.argv[2];
if (!soljson) {
  process.stderr.write("usage: soljson.js <soljson path>\n");
  process.exit(2);
}

const Module = require(soljson);
if (typeof Module.cwrap !== "function" || !Module._solidity_compile) {
  process.stderr.write(`${soljson} does not expose solidity_compile\n`);
  process.exit(2);
}
const compile = Module.cwrap("solidity_compile", "string", [
  "string",
  "number",
  "number",
]);

const chunks = [];
process.stdin.on("data", (chunk) => chunks.push(chunk));
process.stdin.on("end", () => {
  try {
    process.stdout.write(compile(Buffer.concat(chunks).toString("utf8"), 0, 0));
  } catch (err) {
    process.stderr.write(String((err && err.stack) || err));
    process.exit(1);
  }
});
