//! AirPods/Beats mute-gesture handling (macOS 14+).
//!
//! Trigger chain: while an app holds an active record session on the mic,
//! macOS reroutes AirPods/Beats stem presses as *mute commands* aimed at the
//! recording process (`audioaccessoryd` → `audiomxd toggleInputMuteForRecordingProcess:`).
//! If the app has not registered an `AVAudioApplication` input-mute handler,
//! the toggle fails and the system plays a rejection chime plus the
//! "Cannot Control Mic with AirPods …" banner attributed to this app's icon
//! (upstream issue cjpais/Handy#2045). Before the mic is open the same press
//! is a plain play/pause media key — which is exactly why the standing
//! stream used to trigger the banner on every press.
//!
//! The handler registered here (a) stops the active dictation via the
//! transcription coordinator — the sensible "mute" for a dictation app — and
//! (b) returns `true` unconditionally so the rejection banner never plays.
//! See research/airpods-cannot-control-mic-alert-hfp-mechanics.md.
//!
//! Registered once at startup, never unregistered: the handler only fires
//! while an active record session exists, so it is inert the rest of the
//! time. Holding a Bluetooth input stream open is separately prevented (see
//! `managers::audio::should_hold_standing_stream`), which also restores the
//! normal media-key behavior when AirPods provide the mic.
use block2::RcBlock;
use log::{error, info};
use objc2::runtime::{AnyClass, Bool};
use objc2_avf_audio::AVAudioApplication;
use std::sync::Arc;
use tauri::Manager;

use crate::managers::audio::AudioRecordingManager;
use crate::TranscriptionCoordinator;

pub fn register(app_handle: tauri::AppHandle) {
    // AVAudioApplication exists only on macOS 14+, while the app's minimum
    // deployment target is 13.4. The class lookup doubles as the OS version
    // check (same pattern as autostart.rs); calling into a missing class
    // would crash, so skip registration on 13.x.
    if AnyClass::get(c"AVAudioApplication").is_none() {
        info!(
            "AVAudioApplication unavailable (macOS < 14) — AirPods mute-gesture handler not registered"
        );
        return;
    }

    let app_for_block = app_handle.clone();
    let block = RcBlock::new(move |_input_should_be_muted: Bool| -> Bool {
        // Apple calls this block on a system queue — never block it. Detach
        // the coordinator work onto our own thread.
        let app = app_for_block.clone();
        std::thread::spawn(move || {
            // Only act while a recording is actually active; the handler
            // fires for mute-state changes in general.
            if let Some(audio) = app.try_state::<Arc<AudioRecordingManager>>() {
                if audio.is_recording() {
                    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
                        coordinator.request_stop_if_recording("airpods_mute_gesture");
                    } else {
                        error!("TranscriptionCoordinator is not initialized; cannot handle AirPods mute gesture");
                    }
                }
            }
        });
        // Report "handled" unconditionally so audioaccessord/audiomxd never
        // play the "Cannot Control Mic with AirPods …" rejection banner and
        // chime. The boolean arg (inputShouldBeMuted) is irrelevant to us:
        // a dictation app "mutes" by stopping the recording, which the
        // detached thread above performs.
        true.into()
    });

    // SAFETY: `sharedInstance` and `setInputMuteStateChangeHandler_error` are
    // plain ObjC messages with no invariants to uphold beyond the class
    // existing, which the gate above verified. The `Result::Err` case carries
    // the underlying NSError and is logged below.
    let result = unsafe {
        let application = AVAudioApplication::sharedInstance();
        application.setInputMuteStateChangeHandler_error(Some(&*block))
    };
    match result {
        Ok(()) => {
            // Keep the block alive for the lifetime of the process: the
            // registration is never undone and AVFAudio may reference the
            // block beyond this call. A deliberate one-time leak.
            std::mem::forget(block);
            info!("AirPods mute-gesture handler registered (macOS 14+ AVAudioApplication)");
        }
        Err(err) => {
            error!("Failed to register AirPods mute-gesture handler: {err}");
        }
    }
}
