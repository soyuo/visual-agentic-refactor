# Analyzer plugin contract v1

Analyzer plugins run as WebAssembly modules through Wasmi. The runtime exposes no WASI implementation, host imports, file access, network access, or system APIs. The user must install a package, approve it, and enable it in three separate steps. Installation leaves it awaiting approval and disabled; approval resets activation to disabled. Only approved and enabled plugins can execute. Removing a plugin deletes its locally stored package.

## Package manifest

The manifest is JSON with exactly these fields:

```json
{
  "id": "example.kotlin",
  "name": "Example Kotlin analyzer",
  "version": "1.0.0",
  "contractVersion": 1,
  "language": "kotlin",
  "capabilities": ["symbols", "imports", "relationships"],
  "moduleSha256": "<64 lowercase hexadecimal characters>"
}
```

Contract version 1 accepts the language values `kotlin`, `java`, `javascript`, `typescript`, and `python`. The capability list is non-empty and currently accepts `symbols`, `imports`, `relationships`, `controlFlow`, and `annotations`. `moduleSha256` verifies that the module bytes match the manifest. It is an integrity check, not a publisher signature or proof of origin. Users should only approve packages from sources they trust.

## WebAssembly ABI

The module must export linear memory named `memory`, `alloc(i32) -> i32`, and `analyze(i32, i32) -> i64`. The host allocates a UTF-8 JSON input using `alloc`, writes it at the returned pointer, and calls `analyze(inputPointer, inputLength)`. Input JSON has the shape `{"relativePath":"src/File.kt","source":"..."}`. The returned `i64` packs the output pointer in its high 32 bits and output byte length in its low 32 bits. Output is UTF-8 JSON matching Rust `AnalysisOutput` from `apps/desktop/src-tauri/src/analyzer.rs`.

The runtime rejects every host import except the cooperative `env.is_cancelled` function, start functions, invalid graph output, mismatched language or source locations, and results exceeding item limits. It caps the module at 16 MiB, input and output at 2 MiB each, linear memory at 64 MiB, and fuel at 50,000,000 Wasm instructions. Each invocation has a 10-second response deadline and runs on a blocking worker so it does not stall the app runtime. If that deadline expires, the command requests cancellation and returns built-in analyzer output with a partial-result warning. Fuel is a deterministic instruction budget, not a hard wall-clock kill: plugins that poll `env.is_cancelled` stop cooperatively; a non-polling plugin may continue on its worker until it returns or exhausts fuel, and its run slot remains occupied until then.

Plugins receive only the single source string supplied by the host. They cannot read other project files or write to the project. The standard analyzer path remains unchanged when no plugin is selected.

## Compatibility and example

Contract changes require a new `contractVersion`; the host rejects unknown versions and unknown manifest fields. Output must remain within the existing `AnalysisOutput` graph contract. A minimal executable plugin is provided at `examples/analyzer-plugin/minimal.wat`; the Rust ABI test in `apps/desktop/src-tauri/src/analyzer_plugin.rs` runs the same contract. The host-import rejection test demonstrates that declared capabilities do not grant file, network, or other host access.
