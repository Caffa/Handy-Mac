#!/usr/bin/env bun
/**
 * Apple HIG Review Script
 *
 * Feeds Loki reference screenshots to a local vision LLM (Ollama) for
 * evaluation against Apple Human Interface Guidelines.
 *
 * Usage:
 *   bun scripts/hig-review.ts              # Process all screenshots
 *   bun scripts/hig-review.ts --limit 3    # Process only first 3 screenshots
 *   bun scripts/hig-review.ts --model llava # Use a specific model
 *
 * Output:
 *   .loki/hig-review.json  — Structured JSON report
 *   stdout                  — Human-readable summary
 */

import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

// ── Configuration ──────────────────────────────────────────────────────────

const PROJECT_ROOT = resolve(import.meta.dir, "..");
const SCREENSHOTS_DIR = join(PROJECT_ROOT, ".loki", "reference");
const OUTPUT_FILE = join(PROJECT_ROOT, ".loki", "hig-review.json");
const OLLAMA_URL = "http://localhost:11434";
const DEFAULT_MODEL = "qwen3-vl";
const FALLBACK_MODELS = ["llava", "qwen3.5:4b", "qwen3.5:9b"];

const HIG_PROMPT = `You are an Apple Human Interface Guidelines expert reviewing a macOS desktop app UI component. This is a screenshot of a single UI component.

Evaluate against these HIG rules (report only actual violations):
1. Touch target size: interactive elements should be ≥ 44px height
2. Color contrast: text should have ≥ 4.5:1 contrast against background
3. Typography: should use system fonts (SF Pro / -apple-system)
4. Spacing: should follow a consistent grid (ideally 8pt)
5. Corner radius: should be consistent across similar components
6. Visual hierarchy: important elements should be visually prominent
7. Dark mode: if this is a dark-background component, text should be readable
8. Color usage: should use semantic colors, not arbitrary hex values

IMPORTANT: Respond with ONLY valid JSON, no markdown, no explanation outside JSON.
The "component" field MUST be the exact filename provided in the user message.

Respond in this exact JSON format:
{
  "component": "the-filename-here.png",
  "violations": [
    {
      "rule": "<rule name>",
      "severity": "high|medium|low",
      "description": "<what's wrong>",
      "suggestion": "<how to fix>"
    }
  ],
  "overall": "<1-2 sentence summary>"
}

If no violations found, return empty violations array.`;

// ── Types ──────────────────────────────────────────────────────────────────

interface Violation {
  rule: string;
  severity: "high" | "medium" | "low";
  description: string;
  suggestion: string;
}

interface ComponentReview {
  component: string;
  violations: Violation[];
  overall: string;
  error?: string;
}

interface ReviewReport {
  timestamp: string;
  model: string;
  totalScreenshots: number;
  processedScreenshots: number;
  results: ComponentReview[];
  summary: {
    totalViolations: number;
    highSeverity: number;
    mediumSeverity: number;
    lowSeverity: number;
    perComponent: Record<string, number>;
  };
}

// ── CLI argument parsing ───────────────────────────────────────────────────

function parseArgs(): { limit: number; model: string } {
  const args = process.argv.slice(2);
  let limit = Infinity;
  let model = DEFAULT_MODEL;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--limit" && args[i + 1]) {
      limit = parseInt(args[i + 1], 10);
      i++;
    } else if (args[i] === "--model" && args[i + 1]) {
      model = args[i + 1];
      i++;
    }
  }

  return { limit, model };
}

// ── Ollama helpers ─────────────────────────────────────────────────────────

async function checkOllamaRunning(): Promise<boolean> {
  try {
    const res = await fetch(`${OLLAMA_URL}/api/tags`, { signal: AbortSignal.timeout(5000) });
    return res.ok;
  } catch {
    return false;
  }
}

async function getInstalledModels(): Promise<string[]> {
  try {
    const res = await fetch(`${OLLAMA_URL}/api/tags`);
    const data = (await res.json()) as { models: { name: string }[] };
    return data.models.map((m) => m.name);
  } catch {
    return [];
  }
}

async function isVisionModel(modelName: string, installed: string[]): Promise<boolean> {
  // Check if model is installed
  const isInstalled = installed.some((m) => m === modelName || m.startsWith(modelName + ":"));
  if (!isInstalled) return false;

  // Check if it has vision capability via the API
  try {
    const res = await fetch(`${OLLAMA_URL}/api/show`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: modelName }),
    });
    if (res.ok) {
      const data = (await res.json()) as { capabilities?: string[]; details?: { capabilities?: string[] } };
      const caps = data.capabilities || data.details?.capabilities || [];
      if (caps.includes("vision")) return true;
    }
  } catch {
    // Fall through to heuristic
  }

  // Heuristic: models with "vl", "vision", or known vision model names
  const visionKeywords = ["vl", "vision", "llava", "ministral"];
  const lower = modelName.toLowerCase();
  return visionKeywords.some((kw) => lower.includes(kw));
}

async function pullModel(modelName: string): Promise<boolean> {
  console.log(`Pulling model "${modelName}"... This may take a while.`);
  try {
    const res = await fetch(`${OLLAMA_URL}/api/pull`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: modelName, stream: false }),
      signal: AbortSignal.timeout(600_000), // 10 minute timeout for pull
    });
    return res.ok;
  } catch (err) {
    console.error(`Failed to pull model "${modelName}":`, err);
    return false;
  }
}

async function findVisionModel(
  preferredModel: string,
  installed: string[],
): Promise<string | null> {
  // Check preferred model first
  if (await isVisionModel(preferredModel, installed)) {
    return preferredModel;
  }

  // Try fallback models
  for (const fallback of FALLBACK_MODELS) {
    if (await isVisionModel(fallback, installed)) {
      console.log(`Preferred model "${preferredModel}" not available, using "${fallback}".`);
      return fallback;
    }
  }

  // Try all installed models for vision capability
  for (const model of installed) {
    if (await isVisionModel(model, installed)) {
      console.log(`Using installed vision model "${model}".`);
      return model;
    }
  }

  return null;
}

// ── Screenshot processing ──────────────────────────────────────────────────

function getScreenshots(): string[] {
  if (!existsSync(SCREENSHOTS_DIR)) {
    console.error(`Screenshots directory not found: ${SCREENSHOTS_DIR}`);
    console.error("Run `bun run test:visual` to generate Loki reference screenshots first.");
    process.exit(1);
  }

  const files = readdirSync(SCREENSHOTS_DIR)
    .filter((f) => f.endsWith(".png"))
    .sort();

  if (files.length === 0) {
    console.error("No PNG screenshots found in .loki/reference/");
    process.exit(1);
  }

  return files;
}

async function reviewScreenshot(
  filename: string,
  model: string,
): Promise<ComponentReview> {
  const filePath = join(SCREENSHOTS_DIR, filename);
  const imageBuffer = readFileSync(filePath);
  const base64Image = imageBuffer.toString("base64");

  try {
    // Use /api/chat endpoint — works better with modern models like qwen3-vl
    const res = await fetch(`${OLLAMA_URL}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model,
        messages: [
          {
            role: "user",
            content: `${HIG_PROMPT}\n\nFilename: ${filename}`,
            images: [base64Image],
          },
        ],
        stream: false,
        format: {
          type: "object",
          properties: {
            component: { type: "string" },
            violations: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  rule: { type: "string" },
                  severity: { type: "string", enum: ["high", "medium", "low"] },
                  description: { type: "string" },
                  suggestion: { type: "string" },
                },
              },
            },
            overall: { type: "string" },
          },
          required: ["component", "violations", "overall"],
        },
      }),
      signal: AbortSignal.timeout(120_000), // 2 minute timeout per image
    });

    if (!res.ok) {
      const errorText = await res.text();
      return {
        component: filename,
        violations: [],
        overall: `Error: HTTP ${res.status} - ${errorText}`,
        error: `HTTP ${res.status}`,
      };
    }

    const data = (await res.json()) as {
      message?: { content?: string };
      error?: string;
    };

    if (data.error) {
      return {
        component: filename,
        violations: [],
        overall: `Ollama error: ${data.error}`,
        error: data.error,
      };
    }

    // Extract the message content from the chat response
    let responseText = data.message?.content?.trim() ?? "";

    if (!responseText) {
      return {
        component: filename,
        violations: [],
        overall: "Empty response from LLM",
        error: "Empty response",
      };
    }

    // Strip <think>...</think> blocks from reasoning models
    responseText = responseText.replace(/<think>[\s\S]*?<\/think>/g, "").trim();

    // Strip markdown code fences if present
    const jsonMatch = responseText.match(/```(?:json)?\s*\n?([\s\S]*?)\n?```/);
    if (jsonMatch) {
      responseText = jsonMatch[1].trim();
    }

    // Try to parse as JSON
    let parsed: ComponentReview;
    try {
      parsed = JSON.parse(responseText);
    } catch {
      // If JSON parse fails, try to extract JSON from the response
      const extractMatch = responseText.match(/\{[\s\S]*\}/);
      if (extractMatch) {
        try {
          parsed = JSON.parse(extractMatch[0]);
        } catch {
          return {
            component: filename,
            violations: [],
            overall: `Failed to parse LLM response as JSON. Raw: ${responseText.slice(0, 300)}`,
            error: "JSON parse error",
          };
        }
      } else {
        return {
          component: filename,
          violations: [],
          overall: `Failed to parse LLM response. Raw: ${responseText.slice(0, 300)}`,
          error: "JSON parse error",
        };
      }
    }

    // Always use the actual filename as component identifier
    parsed.component = filename;

    // Validate violations array
    if (!Array.isArray(parsed.violations)) {
      parsed.violations = [];
    }

    // Validate each violation
    parsed.violations = parsed.violations
      .filter((v: Record<string, unknown>) => v && typeof v === "object")
      .map((v: Record<string, unknown>) => ({
        rule: String(v.rule ?? "unknown"),
        severity: ["high", "medium", "low"].includes(String(v.severity))
          ? (v.severity as "high" | "medium" | "low")
          : "medium",
        description: String(v.description ?? ""),
        suggestion: String(v.suggestion ?? ""),
      }));

    if (!parsed.overall) {
      parsed.overall = "";
    }

    return parsed;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return {
      component: filename,
      violations: [],
      overall: `Error: ${message}`,
      error: message,
    };
  }
}

// ── Summary ────────────────────────────────────────────────────────────────

function buildSummary(results: ComponentReview[]): ReviewReport["summary"] {
  let totalViolations = 0;
  let highSeverity = 0;
  let mediumSeverity = 0;
  let lowSeverity = 0;
  const perComponent: Record<string, number> = {};

  for (const result of results) {
    const count = result.violations.length;
    totalViolations += count;
    perComponent[result.component] = count;

    for (const v of result.violations) {
      if (v.severity === "high") highSeverity++;
      else if (v.severity === "medium") mediumSeverity++;
      else lowSeverity++;
    }
  }

  return { totalViolations, highSeverity, mediumSeverity, lowSeverity, perComponent };
}

function printSummary(report: ReviewReport): void {
  console.log("\n" + "═".repeat(60));
  console.log("  Apple HIG Review Report");
  console.log("═".repeat(60));
  console.log(`  Model:              ${report.model}`);
  console.log(`  Timestamp:           ${report.timestamp}`);
  console.log(`  Screenshots:         ${report.processedScreenshots}/${report.totalScreenshots}`);
  console.log(`  Total violations:    ${report.summary.totalViolations}`);
  console.log(`  High severity:       ${report.summary.highSeverity}`);
  console.log(`  Medium severity:     ${report.summary.mediumSeverity}`);
  console.log(`  Low severity:        ${report.summary.lowSeverity}`);
  console.log("─".repeat(60));

  // Per-component breakdown (sorted by violation count, descending)
  const sorted = Object.entries(report.summary.perComponent)
    .sort(([, a], [, b]) => b - a);

  if (sorted.length > 0) {
    console.log("  Per-component breakdown:");
    for (const [component, count] of sorted) {
      const bar = "█".repeat(Math.min(count, 20));
      console.log(`    ${component.padEnd(55)} ${count} ${bar}`);
    }
  }

  // Show high-severity violations
  const highViolations = report.results
    .flatMap((r) => r.violations.map((v) => ({ ...v, component: r.component })))
    .filter((v) => v.severity === "high");

  if (highViolations.length > 0) {
    console.log("─".repeat(60));
    console.log("  HIGH SEVERITY VIOLATIONS:");
    for (const v of highViolations) {
      console.log(`    [${v.component}] ${v.rule}: ${v.description}`);
      console.log(`      → ${v.suggestion}`);
    }
  }

  // Show errors
  const errors = report.results.filter((r) => r.error);
  if (errors.length > 0) {
    console.log("─".repeat(60));
    console.log("  ERRORS:");
    for (const e of errors) {
      console.log(`    [${e.component}] ${e.error}`);
    }
  }

  console.log("═".repeat(60));
  console.log(`  Full report saved to: ${OUTPUT_FILE}`);
  console.log("═".repeat(60) + "\n");
}

// ── Main ────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  const { limit, model: requestedModel } = parseArgs();

  // 1. Check if Ollama is running
  console.log("Checking Ollama...");
  const ollamaRunning = await checkOllamaRunning();
  if (!ollamaRunning) {
    console.error("❌ Ollama is not running. Start it with: ollama serve");
    console.error("   Or check if it's listening on http://localhost:11434");
    process.exit(1);
  }
  console.log("✓ Ollama is running");

  // 2. Check for vision model
  console.log("Checking for vision models...");
  const installedModels = await getInstalledModels();
  let visionModel = await findVisionModel(requestedModel, installedModels);

  if (!visionModel) {
    console.log(`No vision model found. Attempting to pull "${DEFAULT_MODEL}"...`);
    const pulled = await pullModel(DEFAULT_MODEL);
    if (pulled) {
      visionModel = DEFAULT_MODEL;
      console.log(`✓ Successfully pulled "${DEFAULT_MODEL}"`);
    } else {
      console.error(
        `❌ Failed to pull "${DEFAULT_MODEL}". Install a vision model manually:\n` +
          `   ollama pull qwen3-vl\n` +
          `   ollama pull llava`,
      );
      process.exit(1);
    }
  } else {
    console.log(`✓ Using vision model: ${visionModel}`);
  }

  // 3. Get screenshots
  const screenshots = getScreenshots();
  const limitedScreenshots = screenshots.slice(0, limit === Infinity ? screenshots.length : limit);
  console.log(
    `Found ${screenshots.length} screenshots, processing ${limitedScreenshots.length}...`,
  );

  // 4. Process each screenshot
  const results: ComponentReview[] = [];
  let processed = 0;

  for (const filename of limitedScreenshots) {
    processed++;
    const progress = `[${processed}/${limitedScreenshots.length}]`;
    process.stdout.write(`${progress} Reviewing ${filename}... `);

    const result = await reviewScreenshot(filename, visionModel);
    results.push(result);

    const violationCount = result.violations.length;
    const errorFlag = result.error ? " ❌" : "";
    console.log(`${violationCount} violation(s)${errorFlag}`);
  }

  // 5. Build report
  const summary = buildSummary(results);
  const report: ReviewReport = {
    timestamp: new Date().toISOString(),
    model: visionModel,
    totalScreenshots: screenshots.length,
    processedScreenshots: limitedScreenshots.length,
    results,
    summary,
  };

  // 6. Save JSON report
  writeFileSync(OUTPUT_FILE, JSON.stringify(report, null, 2));
  console.log(`\nReport saved to ${OUTPUT_FILE}`);

  // 7. Print summary
  printSummary(report);
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});