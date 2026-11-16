/**
 * Voice-insert token text helpers.
 *
 * Inserted tokens arrive embedded in caption text as wikilinks:
 *   [[Target]]           → displays "Target"
 *   [[Target|Alias]]     → displays "Alias"
 *
 * Two consumers:
 * - LiveCaptionsBox splits display text into plain/token parts to render
 *   tokens as colored inline spans without the brackets.
 * - useLiveCaptions' filler-word filter masks tokens into sentinels before
 *   stripping leading filler words, then restores them, so token content can
 *   never be stripped or split by the word-by-word filter.
 */

export interface TokenSpanPart {
  /** Text to display (alias-resolved, brackets stripped). */
  text: string;
  /** Whether this part came from a [[...]] token. */
  isToken: boolean;
}

/** Matches [[...]] with no nested brackets; non-greedy so two adjacent
 * tokens match separately. */
const TOKEN_PATTERN = /\[\[([^\[\]]+?)\]\]/g;

const MASK_PATTERN = /\u0000(\d+)\u0000/g;

/** Alias rule: display only the part after the first pipe, if any. */
function tokenDisplayText(inner: string): string {
  const pipeIndex = inner.indexOf("|");
  return pipeIndex === -1 ? inner : inner.slice(pipeIndex + 1);
}

/**
 * Split display text into plain and token parts. Text outside tokens is
 * returned unchanged; token parts have brackets stripped and the alias rule
 * applied. Unbalanced brackets stay literal.
 */
export function splitTokenSpans(text: string): TokenSpanPart[] {
  const parts: TokenSpanPart[] = [];
  let last = 0;
  for (const match of text.matchAll(TOKEN_PATTERN)) {
    const index = match.index ?? 0;
    if (index > last) {
      parts.push({ text: text.slice(last, index), isToken: false });
    }
    parts.push({ text: tokenDisplayText(match[1]), isToken: true });
    last = index + match[0].length;
  }
  if (last < text.length) {
    parts.push({ text: text.slice(last), isToken: false });
  }
  return parts;
}

export interface MaskedText {
  /** Text with each token display replaced by a \u0000N\u0000 sentinel. */
  masked: string;
  /** The alias-resolved display texts, indexed by sentinel number. */
  tokens: string[];
}

/**
 * Replace every [[...]] token with a sentinel that the filler-word filter
 * cannot match or split (no whitespace, never equal to a filler word).
 */
export function maskTokenSpans(text: string): MaskedText {
  const tokens: string[] = [];
  const masked = text.replace(TOKEN_PATTERN, (_match, inner: string) => {
    tokens.push(tokenDisplayText(inner));
    return `\u0000${tokens.length - 1}\u0000`;
  });
  return { masked, tokens };
}

/**
 * Restore tokens replaced by {@link maskTokenSpans}. Unknown sentinels are
 * dropped rather than leaked into display text.
 */
export function restoreTokenSpans(masked: string, tokens: string[]): string {
  return masked.replace(MASK_PATTERN, (_match, index: string) => {
    return tokens[Number(index)] ?? "";
  });
}
