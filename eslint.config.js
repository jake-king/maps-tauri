// ESLint: the recommended rules (unused variables and imports are the main dead-code signal),
// browser globals for the frontend, Node globals for tests and tools.
import js from "@eslint/js";
import globals from "globals";

export default [
  // logs/ holds scratch files and run output, never source
  { ignores: ["dist/", "node_modules/", "src/wasm/", "target/", "src-tauri/target/", "src-tauri/gen/", "logs/", "book/book/", "book/mermaid.min.js", "book/mermaid-init.js"] },
  js.configs.recommended,
  {
    files: ["src/**/*.js"],
    languageOptions: { ecmaVersion: "latest", sourceType: "module", globals: globals.browser },
  },
  {
    files: ["tests/**/*.js", "tools/**/*.mjs", "*.config.js"],
    languageOptions: { ecmaVersion: "latest", sourceType: "module", globals: globals.node },
  },
  {
    rules: {
      "no-unused-vars": ["error", { args: "after-used", caughtErrors: "none" }],
      eqeqeq: ["error", "always", { null: "ignore" }],
      "prefer-const": "error",
    },
  },
];
