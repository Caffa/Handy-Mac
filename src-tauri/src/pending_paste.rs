//! Delayed paste watcher.
//!
//! When transcription finishes and the focused element is not an editable text
//! field, the text is kept on the clipboard and a watcher arms. It polls the
//! focused element's AXRole and pastes once (and only once) the moment an
//! editable field becomes focused. If AX is unavailable, the focused app cannot
//! be determined, or the user manually pastes, the watcher disarms.

use crate::settings::{get_settings, PasteMethod};
use log::{info, warn};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Manager;

#[cfg(target_os = "macos")]
const POLL_MS: u64 = 250;
#[cfg(target_os = "macos")]
const PENDING_PASTE_TIMEOUT_SECS: u64 = 300;
#[cfg(target_os = "macos")]
const EDITABLE_ROLES: [&str; 4] = ["AXTextArea", "AXTextField", "AXComboBox", "AXSearchField"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Editability {
    Editable,
    NotEditable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmDecision {
    Arm,
    PasteNow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickDecision {
    Fire,
    KeepPending,
    Disarm,
}

/// Decide whether a focused element's role should trigger arming.
///
/// - Editable  → paste now (normal flow)
/// - Unknown   → paste now (AX unavailable/failed/role unknown fallback)
/// - NotEditable → arm the watcher
fn role_editability(role: Option<&str>) -> Editability {
    match role {
        Some(r) if EDITABLE_ROLES.contains(&r) => Editability::Editable,
        Some(_) => Editability::NotEditable,
        None => Editability::Unknown,
    }
}

/// Decide whether to arm the pending-paste watcher at transcription end.
fn decide_arm(enabled: bool, method: &PasteMethod, editability: Editability) -> ArmDecision {
    if !enabled {
        return ArmDecision::PasteNow;
    }
    if matches!(method, PasteMethod::None | PasteMethod::ExternalScript) {
        return ArmDecision::PasteNow;
    }
    match editability {
        Editability::Editable | Editability::Unknown => ArmDecision::PasteNow,
        Editability::NotEditable => ArmDecision::Arm,
    }
}

/// Decide what the watcher thread should do on each poll tick.
fn decide_tick(
    busy_coordinator: bool,
    frontmost_is_self: bool,
    editability: Editability,
    text_already_present: bool,
    enabled_now: bool,
    method_pasteable_now: bool,
) -> TickDecision {
    if !enabled_now || !method_pasteable_now {
        return TickDecision::Disarm;
    }
    // The coordinator transitions Idle asynchronously after a session finishes.
    // A tick that sees Processing/Confirming should wait, not disarm: new-session
    // invalidation is handled by GENERATION/cancel_for_new_session.
    if busy_coordinator {
        return TickDecision::KeepPending;
    }
    if frontmost_is_self {
        return TickDecision::KeepPending;
    }
    match editability {
        Editability::Unknown => TickDecision::KeepPending,
        Editability::Editable => {
            if text_already_present {
                TickDecision::Disarm
            } else {
                TickDecision::Fire
            }
        }
        Editability::NotEditable => TickDecision::KeepPending,
    }
}

/// Slot that holds the text waiting to be pasted and the generation it belongs to.
struct PendingSlot {
    text: String,
    #[cfg(target_os = "macos")]
    armed_at: Instant,
    gen: u64,
}

/// Generation counter. Bumped whenever a new dictation session starts so any
/// still-armed watcher from the previous session exits before it can fire.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The single pending-paste slot. Holding `None` means no watcher is armed.
static SLOT: Mutex<Option<PendingSlot>> = Mutex::new(None);

/// Allocate a fresh generation for a new watcher. `fetch_add` returns the
/// previous value, so the new generation is that value + 1.
fn arm_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// Check whether the global generation still matches the supplied one.
fn gen_is_current(gen: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) == gen
}

/// Arm the delayed-paste watcher if appropriate.
///
/// Returns `true` if the watcher was armed (the caller should skip its normal
/// immediate paste). Returns `false` if the text should be pasted immediately
/// using the existing flow.
pub fn maybe_arm(app_handle: &AppHandle, text: &str) -> bool {
    let settings = get_settings(app_handle);

    if !settings.delayed_paste_enabled {
        return false;
    }

    if matches!(
        settings.paste_method,
        PasteMethod::None | PasteMethod::ExternalScript
    ) {
        return false;
    }

    // Duplicate the trailing-space logic from clipboard::paste() so the pending
    // text that lands on the clipboard matches what an immediate paste would
    // have inserted.
    let final_text = if settings.append_trailing_space {
        format!("{} ", text)
    } else {
        text.to_string()
    };

    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS platforms cannot inspect AX role; fall back to immediate paste.
        let _ = final_text;
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        // Probe non-destructively: peek the saved frontmost app so the normal
        // PasteNow paths (Editable/Unknown) still leave it intact for the
        // desktop-like check and restore_frontmost_app in clipboard::paste().
        let target_pid = if let Some((_, pid)) = crate::focus::peek_saved_frontmost_app(app_handle)
        {
            Some(pid)
        } else {
            frontmost_pid()
        };

        let Some(target_pid) = target_pid else {
            return false;
        };

        let own_pid = std::process::id() as i32;
        if target_pid == own_pid {
            return false;
        }

        // Run the arm-time AX probe off the calling (often main) thread so a
        // hung target app cannot block transcription completion. Timeout treats
        // the element as Unknown, which falls back to immediate paste.
        let editability = probe_editability(target_pid);

        match decide_arm(
            settings.delayed_paste_enabled,
            &settings.paste_method,
            editability,
        ) {
            ArmDecision::PasteNow => false,
            ArmDecision::Arm => {
                // Now consume the saved frontmost app so it is not restored by a
                // later unrelated paste while we hold the text pending.
                let _ = crate::focus::take_saved_frontmost_app(app_handle);

                // Publish the pending text to the clipboard. We intentionally do
                // NOT snapshot the user's current clipboard here: the pending text
                // must survive on the clipboard until fire, and the fire-time
                // verify-then-commit verifier will restore whatever the user
                // copied since arming.
                crate::clipboard::publish_for_pending(&final_text, app_handle);

                // Allocate a fresh generation for this watcher so any previous
                // pending watcher for this session auto-cancels.
                let gen = arm_generation();
                let mut slot = SLOT.lock();
                *slot = Some(PendingSlot {
                    text: final_text.clone(),
                    armed_at: Instant::now(),
                    gen,
                });
                drop(slot);

                info!(
                    "Delayed paste armed for pid={} (timeout={}s)",
                    target_pid, PENDING_PASTE_TIMEOUT_SECS
                );

                let app_handle = app_handle.clone();
                let _ = std::thread::Builder::new()
                    .name("pending-paste-watch".into())
                    .spawn(move || watch(app_handle, gen));

                true
            }
        }
    }
}

/// Cancel any pending paste for the current session. Called when a new recording
/// starts so the previous session's watcher cannot fire into the new one.
pub fn cancel_for_new_session() {
    let new_gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let mut slot = SLOT.lock();
    if slot.is_some() {
        warn!(
            "Cancelling pending paste because a new session started (gen={})",
            new_gen
        );
    }
    *slot = None;
}

/// Clear the slot only if it still belongs to the given generation.
fn clear_if_gen(gen: u64) {
    let mut slot = SLOT.lock();
    if let Some(ref current) = *slot {
        if current.gen == gen {
            *slot = None;
        }
    }
}

#[cfg(target_os = "macos")]
fn frontmost_pid() -> Option<i32> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSWorkspace;

    autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        Some(app.processIdentifier())
    })
}

/// Probe the focused element's editability off the calling thread.
///
/// A timeout (or any AX failure) is treated as Unknown so the caller falls back
/// to immediate paste rather than waiting forever.
#[cfg(target_os = "macos")]
fn probe_editability(target_pid: i32) -> Editability {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    let spawn_result = std::thread::Builder::new()
        .name("pending-paste-probe".into())
        .spawn(move || {
            let info = crate::ax_focus::read_focused_element(target_pid, false);
            let editability = role_editability(info.as_ref().and_then(|i| i.role.as_deref()));
            let _ = tx.send(editability);
        });

    if let Err(e) = spawn_result {
        warn!("Failed to spawn arm-time AX probe thread: {}", e);
        return Editability::Unknown;
    }

    match rx.recv_timeout(Duration::from_millis(300)) {
        Ok(editability) => editability,
        Err(_) => {
            warn!(
                "Arm-time AX probe timed out for pid={} — treating as Unknown",
                target_pid
            );
            Editability::Unknown
        }
    }
}

/// Watcher thread entry point. Polls the focused element until the pending text
/// can be pasted, the user manually pastes, the setting is disabled, a new
/// session starts, or the timeout elapses.
#[cfg(target_os = "macos")]
fn watch(app_handle: AppHandle, gen: u64) {
    let own_pid = std::process::id() as i32;

    loop {
        if !gen_is_current(gen) {
            return;
        }

        let armed_at = {
            let slot = SLOT.lock();
            match &*slot {
                Some(current) if current.gen == gen => current.armed_at,
                _ => return,
            }
        };

        if armed_at.elapsed().as_secs() >= PENDING_PASTE_TIMEOUT_SECS {
            warn!(
                "Delayed paste timed out after {}s",
                PENDING_PASTE_TIMEOUT_SECS
            );
            clear_if_gen(gen);
            return;
        }

        let settings = get_settings(&app_handle);
        let method_pasteable_now = !matches!(
            settings.paste_method,
            PasteMethod::None | PasteMethod::ExternalScript
        );

        if !settings.delayed_paste_enabled || !method_pasteable_now {
            clear_if_gen(gen);
            return;
        }

        let busy = app_handle
            .try_state::<crate::TranscriptionCoordinator>()
            .map(|c| {
                !matches!(
                    c.get_state(),
                    crate::transcription_coordinator::AppState::Idle { .. }
                )
            })
            .unwrap_or(false);

        let Some(target_pid) = frontmost_pid() else {
            std::thread::sleep(Duration::from_millis(POLL_MS));
            continue;
        };

        let frontmost_is_self = target_pid == own_pid;

        let info = crate::ax_focus::read_focused_element(target_pid, true);
        let role = info.as_ref().and_then(|i| i.role.as_deref());
        let editability = role_editability(role);
        let value = info.as_ref().and_then(|i| i.value.as_deref());

        // Read the pending text under the slot lock. If the slot is gone or the
        // generation has moved on, this watcher is done.
        let pending_text = {
            let slot = SLOT.lock();
            match &*slot {
                Some(current) if current.gen == gen => current.text.clone(),
                _ => return,
            }
        };

        let text_already_present = value.is_some_and(|v| {
            v.contains(&pending_text)
                || (pending_text.ends_with(char::is_whitespace)
                    && v.contains(pending_text.trim_end()))
        });

        match decide_tick(
            busy,
            frontmost_is_self,
            editability,
            text_already_present,
            settings.delayed_paste_enabled,
            method_pasteable_now,
        ) {
            TickDecision::Fire => {
                // Re-check generation right before firing to shrink the
                // cancel-after-decide race window.
                if !gen_is_current(gen) {
                    return;
                }

                info!("Focused element is editable — firing delayed paste");
                if let Err(e) = crate::clipboard::paste_pending_now(&pending_text, &app_handle) {
                    warn!(
                        "Delayed paste fire failed ({}); keeping text on clipboard",
                        e
                    );
                    let _ = crate::clipboard::write_to_clipboard(&pending_text, &app_handle);
                }
                clear_if_gen(gen);
                return;
            }
            TickDecision::Disarm => {
                info!("Delayed paste disarmed (manual paste or setting disabled)");
                clear_if_gen(gen);
                return;
            }
            TickDecision::KeepPending => {
                std::thread::sleep(Duration::from_millis(POLL_MS));
                continue;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_roles_are_recognized() {
        assert_eq!(role_editability(Some("AXTextArea")), Editability::Editable);
        assert_eq!(role_editability(Some("AXTextField")), Editability::Editable);
        assert_eq!(role_editability(Some("AXComboBox")), Editability::Editable);
        assert_eq!(
            role_editability(Some("AXSearchField")),
            Editability::Editable
        );
    }

    #[test]
    fn non_editable_and_unknown_roles() {
        assert_eq!(role_editability(Some("AXGroup")), Editability::NotEditable);
        assert_eq!(role_editability(Some("AXButton")), Editability::NotEditable);
        assert_eq!(role_editability(None), Editability::Unknown);
    }

    #[test]
    fn decide_arm_matrix() {
        for method in [
            PasteMethod::CtrlV,
            PasteMethod::CtrlShiftV,
            PasteMethod::ShiftInsert,
            PasteMethod::Direct,
        ] {
            assert_eq!(
                decide_arm(true, &method, Editability::Editable),
                ArmDecision::PasteNow
            );
            assert_eq!(
                decide_arm(true, &method, Editability::Unknown),
                ArmDecision::PasteNow
            );
            assert_eq!(
                decide_arm(true, &method, Editability::NotEditable),
                ArmDecision::Arm
            );
            assert_eq!(
                decide_arm(false, &method, Editability::NotEditable),
                ArmDecision::PasteNow
            );
        }

        assert_eq!(
            decide_arm(true, &PasteMethod::None, Editability::NotEditable),
            ArmDecision::PasteNow
        );
        assert_eq!(
            decide_arm(true, &PasteMethod::ExternalScript, Editability::NotEditable),
            ArmDecision::PasteNow
        );
    }

    #[test]
    fn decide_tick_matrix() {
        // Basic editable fire.
        assert_eq!(
            decide_tick(false, false, Editability::Editable, false, true, true),
            TickDecision::Fire
        );

        // Manual paste disarms.
        assert_eq!(
            decide_tick(false, false, Editability::Editable, true, true, true),
            TickDecision::Disarm
        );

        // Busy coordinator keeps pending until the async Idle transition.
        assert_eq!(
            decide_tick(true, false, Editability::Editable, false, true, true),
            TickDecision::KeepPending
        );

        // Self frontmost keeps pending.
        assert_eq!(
            decide_tick(false, true, Editability::NotEditable, false, true, true),
            TickDecision::KeepPending
        );

        // Unknown keeps pending.
        assert_eq!(
            decide_tick(false, false, Editability::Unknown, false, true, true),
            TickDecision::KeepPending
        );

        // Disabled or non-pasteable method disarms.
        assert_eq!(
            decide_tick(false, false, Editability::NotEditable, false, false, true),
            TickDecision::Disarm
        );
        assert_eq!(
            decide_tick(false, false, Editability::NotEditable, false, true, false),
            TickDecision::Disarm
        );

        // Not editable keeps pending.
        assert_eq!(
            decide_tick(false, false, Editability::NotEditable, false, true, true),
            TickDecision::KeepPending
        );
    }

    #[test]
    fn default_settings_have_delayed_paste_enabled() {
        let settings = crate::settings::get_default_settings();
        assert!(settings.delayed_paste_enabled);
    }

    #[test]
    fn missing_field_defaults_to_delayed_paste_enabled() {
        let settings = serde_json::from_str::<crate::settings::AppSettings>("{}")
            .expect("empty JSON should deserialize with defaults");
        assert!(settings.delayed_paste_enabled);
    }

    #[test]
    fn generation_handoff_roundtrip() {
        let gen = arm_generation();
        assert!(gen_is_current(gen));

        cancel_for_new_session();
        assert!(!gen_is_current(gen));
    }
}
