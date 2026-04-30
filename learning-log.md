# Handy-Fork Learning Log

## USB Watchdog Power-Cycle Bug (2026-04-28)

### Problem
- When dictation is active and the USB microphone dies, Handy should power-cycle the USB hub via uhubctl and retry
- The transcription overlay visualizer would get stuck (frozen bars) when mic-level events stopped arriving
- Handy could crash because the power-cycle was async but the retry happened immediately (before the device re-enumerated)

### Root Causes

1. **`power_cycle()` was fire-and-forget (spawning a thread)**: The `cycling` flag was set to `true`, a thread was spawned to run uhubctl + 12s settle, but then `cycling` was **immediately set back to `false`** on the same line (line 178 original). This meant:
   - The caller (`start_microphone_stream` → `on_mic_open_failed`) returned `true` saying "cycle initiated" but the cycle hadn't happened yet
   - The retry happened immediately while the device was still offline
   - No cooldown protection against double-cycling

2. **`force_power_cycle()` had the same bug**: Cycling flag cleared immediately after `thread::spawn`, before the actual cycle completed

3. **Overlay visualizer had no level decay**: When mic-level events stopped (dead USB stream), the bars just froze at their last values with no fallback

4. **No frontend feedback during USB cycling**: User had no idea the device was being power-cycled

### Fixes

1. **Made `power_cycle()` → `power_cycle_blocking()`**: Runs uhubctl + settle synchronously on the calling thread. This ensures the mic-open retry actually finds the device re-enumerated. Called from `on_mic_open_failed()`.

2. **`force_power_cycle()` (UI-triggered)**: Uses `Arc<AtomicBool>` for the `cycling` flag so the spawned thread can properly clear it after completion. No raw pointers.

3. **Added `AppHandle` to `UsbWatchdog`**: Emits `usb-power-cycle-started`, `usb-power-cycle-finished`, and `usb-power-cycle-failed` Tauri events to the frontend.

4. **Overlay: level decay timer**: Added an 80ms interval that decays bar heights toward zero when no `mic-level` events arrive for 500ms (dead stream detection). Prevents frozen bars.

5. **Overlay: USB cycling state**: Added `"usb-cycling"` overlay state with gold pulsing text "USB cycling…" so the user knows what's happening.

6. **Settings UI**: Updated `UsbWatchdog.tsx` to listen for events instead of hardcoded timeouts.

7. **Added `is_cycling()` method**: Public API for checking if a cycle is in progress.

### Key Insight
When `std::thread::spawn` is used for async work, **never clear state flags immediately after the spawn call**. Either:
- Make the operation blocking (simple, correct)
- Use `Arc<AtomicX>` shared state that the spawned thread clears on completion