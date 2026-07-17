/** @type {import('stylelint').Config} */
module.exports = {
  extends: ["stylelint-config-standard"],
  rules: {
    // ── Relax formatting rules that conflict with Tailwind CSS 4 ──
    "comment-empty-line-before": null,
    "custom-property-empty-line-before": null,
    "declaration-empty-line-before": null,
    "rule-empty-line-before": null,

    // Allow Tailwind CSS 4 at-rules (@theme, @layer, @apply, etc.)
    "annotation-no-unknown": [
      true,
      { ignoreAnnotations: ["container"] },
    ],
    "at-rule-no-unknown": [
      true,
      {
        ignoreAtRules: [
          "theme",
          "layer",
          "apply",
          "tailwind",
          "variants",
          "responsive",
          "screen",
        ],
      },
    ],

    // ── Apple HIG-Inspired Rules (lenient: warnings, not errors) ──

    // z-index should only use numeric values or CSS variables
    // (prevents arbitrary z-index arms races per Apple HIG)
    "declaration-property-value-allowed-list": {
      "z-index": [/^-?\d+$/, /^var\(/],
    },

    // Disallow hard-coded font families that aren't system fonts.
    // This catches "Arial", "Helvetica", "Times New Roman", etc.
    // while allowing -apple-system, BlinkMacSystemFont, system-ui, sans-serif,
    // monospace, and Tailwind's font-family utilities.
    "declaration-property-value-disallowed-list": {
      "font-family": [
        /^Arial$/,
        /^Helvetica$/,
        /^"Helvetica Neue"$/,
        /^"Times New Roman"$/,
        /^Georgia$/,
        /^Verdana$/,
        /^Courier New$/,
        /^Comic Sans MS$/,
        /^Impact$/,
      ],
    },

    // Enforce 6-digit hex colors for consistency (warn only)
    "color-hex-length": [
      "long",
      { severity: "warning" },
    ],

    // Modern color notation — warn only (rgba → rgb modern syntax, percentage alpha)
    "color-function-notation": [
      "modern",
      { severity: "warning" },
    ],
    // Disable alias notation rule — color-function-notation already covers
    // modernizing rgba() → rgb(). The alias rule doesn't accept severity overrides.
    "color-function-alias-notation": null,
    "alpha-value-notation": [
      "percentage",
      { severity: "warning" },
    ],

    // value-keyword-case: warn only (lowercase preferred but not enforced)
    "value-keyword-case": [
      "lower",
      { severity: "warning" },
    ],

    // Allow only standard units (warn for unusual units like cm, mm, in, pt, pc)
    "unit-allowed-list": [
      ["px", "rem", "em", "%", "vh", "vw", "deg", "s", "ms", "fr", "dvh", "svh"],
      { severity: "warning" },
    ],

    // Zero values should not have units (e.g. 0px → 0), warn only
    "length-zero-no-unit": [
      true,
      { severity: "warning" },
    ],

    // Deprecated CSS properties: warn only (e.g. word-wrap → overflow-wrap)
    "property-no-deprecated": [
      true,
      { severity: "warning" },
    ],
    "declaration-property-value-keyword-no-deprecated": [
      true,
      { severity: "warning" },
    ],

    // Keep CSS flat — max 3 levels of nesting
    "max-nesting-depth": [
      3,
      {
        severity: "warning",
        message:
          "Keep CSS flat for maintainability. Nesting beyond 3 levels increases specificity and reduces readability. (Apple HIG)",
      },
    ],

    // Warn on descending specificity (common source of style bugs)
    "no-descending-specificity": [
      true,
      {
        severity: "warning",
        message:
          "Descending specificity can cause unexpected style overrides. Consider reordering or reducing specificity. (Apple HIG)",
      },
    ],

    // Kebab-case + allow Tailwind utility classes (colons, brackets)
    "selector-class-pattern": [
      /^[a-z][a-z0-9]*(-[a-z0-9]+)*(\[.*\])?(:[a-z]+)?$/,
      {
        severity: "warning",
        message:
          "Prefer kebab-case class names. Tailwind utility classes (with colons/brackets) are allowed.",
      },
    ],

    // Warn on duplicate selectors
    "no-duplicate-selectors": [true, { severity: "warning" }],

    // Always include a generic font family keyword as fallback
    "font-family-no-missing-generic-family-keyword": [
      true,
      {
        severity: "warning",
        message:
          "Always include a generic font family keyword (sans-serif, monospace) as fallback. (Apple HIG)",
      },
    ],

    // Warn on vendor prefixes (keep -webkit- for backdrop-filter etc.)
    "property-no-vendor-prefix": [
      true,
      {
        severity: "warning",
        ignoreProperties: ["backdrop-filter", "text-size-adjust", "font-smoothing"],
      },
    ],
    "value-no-vendor-prefix": [true, { severity: "warning" }],

    // Warn on duplicate properties (allow consecutive with different values for fallbacks)
    "declaration-block-no-duplicate-properties": [
      true,
      {
        severity: "warning",
        ignore: ["consecutive-duplicates-with-different-values"],
      },
    ],

    // ── Disable rules that don't apply to this project ──
    "declaration-block-single-line-max-declarations": null,
    "block-no-empty": null,
    "selector-max-id": null,
    "import-notation": null,
    "keyframes-name-pattern": null,
    "no-invalid-position-at-import-rule": null,
    "selector-not-notation": null,
    "selector-no-vendor-prefix": null,
    "media-feature-name-no-vendor-prefix": null,

    // Don't disallow hex colors (HIG uses them in design tokens)
    "color-no-hex": null,
  },
};