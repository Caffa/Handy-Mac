// actions — Keyboard shortcut action handlers for transcription, routing,
// and cancellation. Splits into sub-modules by concern:
//   mod.rs          — ShortcutAction trait, ACTION_MAP, CancelAction, TestAction
//   transcribe.rs   — TranscribeAction (standard + post-process)
//   router.rs       — TranscribeWithRouterAction (boss_router.py)
//   post_process.rs — LLM post-processing, Chinese conversion, transcription pipeline

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;

pub mod post_process;
pub mod router;
pub mod transcribe;

// Re-export public items so external code using `crate::actions::X` continues to work
pub use post_process::process_transcription_output;

// ── Shortcut Action Trait ──

pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// ── Cancel Action ──

struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        crate::utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// ── Test Action ──

struct TestAction;

impl ShortcutAction for TestAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Started - {} (App: {})",
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Stopped - {} (App: {})",
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }
}

// ── Static Action Map ──

pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "transcribe".to_string(),
        Arc::new(transcribe::TranscribeAction {
            post_process: false,
        }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_post_process".to_string(),
        Arc::new(transcribe::TranscribeAction { post_process: true }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_router".to_string(),
        Arc::new(router::TranscribeWithRouterAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map
});
