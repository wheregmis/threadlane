const fs = require("node:fs");
const assert = require("node:assert/strict");

WebAssembly.instantiate(fs.readFileSync(process.argv[2]), {})
  .then(({ instance }) => {
    assert.equal(instance.exports.check_grammars(), 4);
    console.log("WASM runtime: Rust, JSON, Bash command and Bash heredoc passed");
  })
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
