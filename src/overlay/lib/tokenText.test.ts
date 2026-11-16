import assert from "node:assert/strict";
import {
  maskTokenSpans,
  restoreTokenSpans,
  splitTokenSpans,
} from "./tokenText";

// Plain text without tokens passes through unchanged.
{
  const parts = splitTokenSpans("just regular caption text");
  assert.equal(parts.length, 1);
  assert.equal(parts[0].isToken, false);
  assert.equal(parts[0].text, "just regular caption text");
}

// Simple token: brackets stripped, single colored part.
{
  const parts = splitTokenSpans("hello [[Alice]] world");
  assert.deepEqual(
    parts.map((p) => [p.isToken, p.text]),
    [
      [false, "hello "],
      [true, "Alice"],
      [false, " world"],
    ],
  );
}

// Alias rule: [[Target|Alias]] displays only Alias; [[Target]] displays Target.
{
  const parts = splitTokenSpans("[[Char - Alice|Alice]] and [[Bob]]");
  assert.deepEqual(
    parts.filter((p) => p.isToken).map((p) => p.text),
    ["Alice", "Bob"],
  );
  assert.equal(
    parts.map((p) => p.text).join(""),
    "Alice and Bob",
  );
}

// Multiple adjacent tokens split separately (non-greedy match).
{
  const parts = splitTokenSpans("[[A]][[B]]");
  assert.deepEqual(
    parts.map((p) => [p.isToken, p.text]),
    [
      [true, "A"],
      [true, "B"],
    ],
  );
}

// Unbalanced brackets stay literal.
{
  const parts = splitTokenSpans("oops [[Open and [[Closed]]");
  const tokenParts = parts.filter((p) => p.isToken);
  assert.equal(tokenParts.length, 1);
  assert.equal(tokenParts[0].text, "Closed");
  assert.equal(parts.some((p) => p.text.includes("[[Open")), true);
}

// Token at the very start and very end.
{
  const parts = splitTokenSpans("[[Start]] mid [[End]]");
  assert.equal(parts[0].isToken, true);
  assert.equal(parts[parts.length - 1].isToken, true);
  assert.equal(parts.map((p) => p.text).join(""), "Start mid End");
}

// Empty string yields no parts.
assert.deepEqual(splitTokenSpans(""), []);

// ── mask/restore roundtrip guards tokens through the filler filter ──

// A token adjacent to filler words is neither stripped nor split, even when
// it is the first word (which the filter would otherwise drop). Restore
// yields display text — brackets are syntax and stay stripped.
{
  const original = "[[Alice]] um hello";
  const { masked, tokens } = maskTokenSpans(original);
  assert.ok(!masked.includes("Alice"), "token hidden from the filter");
  // Simulate the word-by-word filler filter: leading word is a sentinel
  // (not a filler), so everything survives the split/join roundtrip.
  const filtered = masked.trim().split(/\s+/).join(" ");
  assert.equal(restoreTokenSpans(filtered, tokens), "Alice um hello");
}

// A filler before the token is still stripped; the token survives intact.
{
  const original = "so [[Note A]] and [[Char - Bob|Bob]]";
  const { masked, tokens } = maskTokenSpans(original);
  const words = masked.trim().split(/\s+/);
  // First word is the bare filler "so" → filter drops it, like the real
  // filterStreamingText does for a single leading filler.
  const filtered = words.slice(1).join(" ");
  assert.equal(restoreTokenSpans(filtered, tokens), "Note A and Bob");
}

// Whitespace inside token brackets cannot leak into the masked form: the
// masked replacement never contains spaces, so split(/\s+/) keeps it whole.
{
  const { masked } = maskTokenSpans("a [[Char - Alice|Alice]] b");
  assert.ok(!/\s/.test(matchSentinel(masked)), "sentinel has no whitespace");
}

function matchSentinel(masked: string): string {
  const m = masked.match(/\u0000\d+\u0000/);
  return m ? m[0] : "";
}

console.log("tokenText: all assertions passed");
