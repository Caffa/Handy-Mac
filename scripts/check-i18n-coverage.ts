import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const SRC_DIR = path.join(__dirname, "..", "src");
const EN_TRANSLATION_PATH = path.join(
  __dirname,
  "..",
  "src",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

interface ExtractedKey {
  key: string;
  file: string;
  line: number;
  column: number;
}

interface MissingKeyReport {
  key: string;
  occurrences: ExtractedKey[];
}

interface CoverageResult {
  ok: boolean;
  scannedFiles: number;
  extractedKeys: number;
  uniqueKeys: number;
  missingKeys: MissingKeyReport[];
}

interface CliOptions {
  json: boolean;
  ignoreFiles: string[];
}

const colors: Record<string, string> = {
  reset: "\x1b[0m",
  red: "\x1b[31m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  blue: "\x1b[34m",
  cyan: "\x1b[36m",
};

function colorize(text: string, color: string): string {
  return `${colors[color]}${text}${colors.reset}`;
}

/**
 * Files that build keys dynamically or are otherwise known to be safe.
 * Keep this list minimal and document each entry.
 */
const BUILT_IN_IGNORED_FILES: string[] = [
  // Builds `onboarding.models.${model.id}.{name,description}` from runtime data.
  "src/lib/utils/modelTranslation.ts",
];

function parseArgs(): CliOptions {
  const args = process.argv.slice(2);
  const json = args.includes("--json");
  const ignoreFlagIndex = args.findIndex((arg) => arg === "--ignore");
  const ignoreFiles: string[] =
    ignoreFlagIndex !== -1 && args[ignoreFlagIndex + 1]
      ? args[ignoreFlagIndex + 1].split(",").map((s) => s.trim())
      : [];
  return { json, ignoreFiles };
}

function getIgnoredFiles(userFiles: string[]): Set<string> {
  return new Set([...BUILT_IN_IGNORED_FILES, ...userFiles]);
}

function isIgnoredFile(relativePath: string, ignored: Set<string>): boolean {
  if (ignored.has(relativePath)) return true;
  for (const pattern of ignored) {
    if (pattern.includes("*") || pattern.includes("?")) {
      const regex = new RegExp(
        `^${pattern.replace(/\./g, "\\.").replace(/\*/g, ".*").replace(/\?/g, ".")}$`,
      );
      if (regex.test(relativePath)) return true;
    }
  }
  return false;
}

function findSourceFiles(dir: string): string[] {
  const results: string[] = [];
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      results.push(...findSourceFiles(fullPath));
    } else if (
      entry.isFile() &&
      (entry.name.endsWith(".ts") || entry.name.endsWith(".tsx")) &&
      !entry.name.endsWith(".d.ts") &&
      !entry.name.endsWith(".stories.tsx")
    ) {
      results.push(fullPath);
    }
  }
  return results;
}

function normalizeKey(key: string): string {
  // Trim accidental whitespace in source literals.
  return key.trim();
}

function flattenKeys(obj: unknown, prefix: string[] = []): string[] {
  if (typeof obj !== "object" || obj === null || Array.isArray(obj)) {
    return [prefix.join(".")];
  }
  const record = obj as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length === 0) {
    return [prefix.join(".")];
  }
  const result: string[] = [];
  for (const key of keys) {
    result.push(...flattenKeys(record[key], [...prefix, key]));
  }
  return result;
}

function loadEnglishKeys(): Set<string> {
  const content = fs.readFileSync(EN_TRANSLATION_PATH, "utf8");
  const data = JSON.parse(content) as unknown;
  return new Set(flattenKeys(data));
}

function stripComments(source: string): string {
  // Remove single-line comments and multi-line comments while preserving line counts.
  return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
}

function getLineAndColumn(
  source: string,
  offset: number,
): { line: number; column: number } {
  let line = 1;
  let lastNewline = -1;
  for (let i = 0; i < offset && i < source.length; i++) {
    if (source[i] === "\n") {
      line++;
      lastNewline = i;
    }
  }
  return { line, column: offset - lastNewline };
}

/**
 * Extract static translation keys from a source file.
 *
 * Supported patterns:
 * - t("key") / t('key')
 * - i18n.t("key") / i18n.t('key')
 * - i18nKey="key" / i18nKey='key'
 *
 * Only single-line string literals are captured. Template literals with
 * interpolations and variables are intentionally skipped.
 */
function extractKeysFromFile(filePath: string): ExtractedKey[] {
  const rawContent = fs.readFileSync(filePath, "utf8");
  const content = stripComments(rawContent);
  const keys: ExtractedKey[] = [];

  const tCallPattern = /(?:\bi18n\.t|\bt)\s*\(\s*(['"])([^'"]*?)\1/gs;
  const i18nKeyPattern = /\bi18nKey\s*=\s*(['"])([^'"]*?)\1/gs;

  for (const match of content.matchAll(tCallPattern)) {
    const { line, column } = getLineAndColumn(rawContent, match.index);
    keys.push({
      key: normalizeKey(match[2]),
      file: filePath,
      line,
      column,
    });
  }

  for (const match of content.matchAll(i18nKeyPattern)) {
    const { line, column } = getLineAndColumn(rawContent, match.index);
    keys.push({
      key: normalizeKey(match[2]),
      file: filePath,
      line,
      column,
    });
  }

  return keys;
}

function groupByKey(keys: ExtractedKey[]): MissingKeyReport[] {
  const map = new Map<string, ExtractedKey[]>();
  for (const occurrence of keys) {
    const existing = map.get(occurrence.key) ?? [];
    existing.push(occurrence);
    map.set(occurrence.key, existing);
  }
  return Array.from(map.entries())
    .map(([key, occurrences]) => ({ key, occurrences }))
    .sort((a, b) => a.key.localeCompare(b.key));
}

function checkCoverage(options: CliOptions): CoverageResult {
  const englishKeys = loadEnglishKeys();
  const sourceFiles = findSourceFiles(SRC_DIR);
  const ignored = getIgnoredFiles(options.ignoreFiles);

  let allExtracted: ExtractedKey[] = [];
  let scannedFiles = 0;

  for (const filePath of sourceFiles) {
    const relativePath = path.relative(process.cwd(), filePath);
    if (isIgnoredFile(relativePath, ignored)) {
      continue;
    }

    scannedFiles++;
    const extracted = extractKeysFromFile(filePath);
    for (const item of extracted) {
      item.file = relativePath;
      allExtracted.push(item);
    }
  }

  const missing: ExtractedKey[] = allExtracted.filter(
    (item) => !englishKeys.has(item.key),
  );

  return {
    ok: missing.length === 0,
    scannedFiles,
    extractedKeys: allExtracted.length,
    uniqueKeys: new Set(allExtracted.map((item) => item.key)).size,
    missingKeys: groupByKey(missing),
  };
}

function printTextReport(result: CoverageResult): void {
  console.log(colorize("\n🌍 i18n Coverage Check\n", "blue"));
  console.log(`Scanned ${result.scannedFiles} source files`);
  console.log(
    `Extracted ${result.extractedKeys} keys (${result.uniqueKeys} unique)\n`,
  );

  if (result.ok) {
    console.log(
      colorize("✓ All referenced translation keys exist in English", "green"),
    );
    return;
  }

  console.log(
    colorize(
      `✗ Missing ${result.missingKeys.length} translation keys:\n`,
      "red",
    ),
  );

  for (const report of result.missingKeys) {
    console.log(colorize(`  ${report.key}`, "yellow"));
    for (const occurrence of report.occurrences) {
      console.log(`    ${occurrence.file}:${occurrence.line}`);
    }
  }

  console.log("");
  console.log(
    colorize(
      `✗ Coverage check failed: ${result.missingKeys.length} referenced keys are missing from English translations`,
      "red",
    ),
  );
}

function main(): void {
  const options = parseArgs();
  const result = checkCoverage(options);

  if (options.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    printTextReport(result);
  }

  process.exit(result.ok ? 0 : 1);
}

main();
