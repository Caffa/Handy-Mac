# Visualizer Animation Principles

Status: Active — Documents the current visualizer bar animation system.

## Overview

The audio visualizer displays 9 vertical bars representing real-time frequency
analysis of microphone input. The animation is designed to feel organic and
responsive while avoiding abrupt, "digital" motion.

## Architecture

### Data Flow

```
Mic → cpal → FFT (visualizer.rs) → mic-level event → useVisualizer.ts → VisualizerBars.tsx → CSS
```

### Layers

| Layer | File | Role |
|-------|------|------|
| FFT Analysis | `visualizer.rs` | Frequency decomposition, peak tracking, auto-gain |
| Smoothing | `useVisualizer.ts` | Adaptive EMA, decay timer, mic health detection |
| Rendering | `VisualizerBars.tsx` | Height calculation, CSS transition timing |
| Styling | `RecordingOverlay.css` | Bar dimensions, container layout |

## Animation Principles Applied

### 1. Wider Elements Feel Heavier

**Principle:** Thicker bars feel less "twitchy" because the eye tracks wider
elements more slowly. This is a well-known animation principle — larger/heavier
objects appear to move more slowly even at the same speed.

**Implementation:** Bar width increased from 8px to 10px. The visual difference
is significant despite only 2px change.

### 2. Asymmetric Timing (Rise ≠ Fall)

**Principle:** Exiting is slower than entering. Bars should rise quickly to
respond to speech, but decay more gradually to avoid abrupt collapse.

**Implementation:**
- Rise: 120ms — fast enough to feel responsive
- Fall: 100ms — slightly faster than rise, feels natural without being twitchy

The timings are close to equal because the CSS easing curves handle the
perceptual difference (see below).

### 3. Easing Curves (Cubic-Bezier)

**Principle:** Linear motion feels robotic. Cubic-bezier curves add
acceleration/deceleration that mimics natural motion.

**Implementation:**
- Rise: `cubic-bezier(0.25, 0.46, 0.45, 0.94)` — ease-out-quad
  - Starts fast, decelerates gently at the top
  - Creates a soft "landing" at peak height
- Fall: `cubic-bezier(0.4, 0, 0.6, 1)` — ease-in-out
  - Smooth acceleration into decay, gentle deceleration at bottom
  - Avoids the "stuck at max" feeling

### 4. Power Curve for Perceptual Balance

**Principle:** Human hearing is logarithmic. Small volume changes at low levels
are more noticeable than the same changes at high levels.

**Implementation:** `height = min(35, 7 + v^0.7 * 28)`
- Power curve 0.7 compresses the top range
- Small values get more visual space (7px minimum ensures visibility)
- Prevents bars from jumping to max too easily

### 5. Adaptive Smoothing

**Principle:** Different contexts need different responsiveness:
- Silence → speech: needs fast response (catch the start of words)
- Steady speech: moderate smoothing reduces jitter
- Quiet speech: slightly more responsive to catch soft sounds

**Implementation:** Adaptive EMA with three alpha modes:
- Rise from silence (prev < 0.05): alpha = 0.75
- Barely moving (delta < 0.02): alpha = 0.65
- Default: alpha = 0.6

### 6. Spectral Variation Preservation

**Principle:** Different frequency bands (bass, mid, treble) should show
meaningful height differences. A "wall" of identical bars looks artificial.

**Implementation:**
- Inter-bucket smoothing: 0.9/0.05/0.05 (keeps 90% of each bucket's value)
- Auto-gain boost capped at 2.5x (prevents all bars maxing out)
- Peak tracking with slow decay (4s half-life) preserves dynamic range

## Current Timing Values

| Parameter | Value | Notes |
|-----------|-------|-------|
| Rise duration | 120ms | Ease-out-quad |
| Fall duration | 100ms | Ease-in-out |
| Bar width | 10px | Wider = less twitchy |
| Bar gap | 4px | Scales with width |
| Container height | 30px | Aligns with icon bottom |
| Max bar height | 35px | Clipped by container |
| Min bar height | 7px | Always visible |
| Height power curve | v^0.7 | Perceptual compression |
| Smoothing alpha | 0.6–0.75 | Adaptive EMA |
| Auto-gain max | 2.5x | Prevents maxing out |
| Inter-bucket blend | 0.9/0.05/0.05 | Preserves variation |

## Tuning Guide

### If bars feel too "twitchy"
- Increase bar width (10px → 12px)
- Increase smoothing alpha slightly (0.6 → 0.65)

### If bars feel too slow
- Decrease rise duration (120ms → 100ms)
- Increase smoothing alpha (0.6 → 0.7)

### If all bars look the same height
- Reduce auto-gain max (2.5x → 2.0x)
- Reduce inter-bucket smoothing (0.9 → 0.95)

### If bars jump to max too easily
- Increase power curve exponent (0.7 → 0.8)
- Reduce auto-gain max (2.5x → 2.0x)

### If fall feels "stuck at max"
- Decrease fall duration (100ms → 80ms)
- Change fall easing to more aggressive curve

## Comparison with Upstream

| Aspect | Upstream (cjpais/Handy) | Fork (Handy-Mac) |
|--------|------------------------|------------------|
| Bar width | 4px | 10px |
| Max height | 18px | 35px |
| Smoothing alpha | 0.3 | 0.6–0.75 |
| CSS timing | 80ms linear | 120ms/100ms cubic-bezier |
| Auto-gain | None | 2.5x |
| Peak tracking | None | Yes (4s decay) |
| Inter-bucket | 0.7/0.15 | 0.9/0.05 |

The fork's visualizer is more responsive and visually prominent, with
sophisticated adaptive behavior that the upstream lacks.
