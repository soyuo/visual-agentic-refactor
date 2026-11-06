# Visual Agentic Refactor

Tauri 2 desktop app with a SolidJS and TypeScript frontend and a Rust core.

## Development

Install Node.js and npm for frontend development and bundling, then run:

```sh
npm ci
npm run tauri:dev
```

## Desktop bundle

```sh
npm run tauri:build
```

Tauri runs `npm run build` and packages the generated `dist/` files. Node.js,
npm, Vite, and a development server are build-time tools; the packaged app loads
static frontend assets and does not require a Node.js server at runtime.
