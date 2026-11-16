import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";

// Lean starter config: type-unaware (fast, no project service) recommended
// rules for TS + React hooks. The opinionated react-compiler / "you might not
// need an effect" rules are surfaced as warnings, not errors — they flag
// existing working patterns and are better addressed as a focused refactor than
// as a CI blocker. Tighten over time (type-checked rules, a11y, promote the
// warnings to errors) once the baseline is green. Test files use their own
// tsconfig and are out of scope here; build output is ignored.
export default tseslint.config(
  { ignores: ["dist", "coverage", "node_modules"] },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      globals: globals.browser,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      // Honor the `_`-prefix "intentionally unused" convention.
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      // NBSP/ZWSP inside regex literals is intentional — the visualize()
      // highlighter in CorrectionsDiff matches those code points on purpose.
      "no-irregular-whitespace": ["error", { skipRegExps: true }],
      // Opinionated rules flagging existing, working patterns — warn for now.
      "react-hooks/set-state-in-effect": "warn",
      "react-hooks/immutability": "warn",
      "react-hooks/preserve-manual-memoization": "warn",
      "@typescript-eslint/no-explicit-any": "warn",
      "no-useless-assignment": "warn",
    },
  },
);
