<!-- Status: Historical — One-time UI/UX audit from June 2026. Read for context, not as a current TODO list. -->

# Handy-Mac UI/UX Review Report

**Date:** June 28, 2026  
**Reviewer:** UI/UX Review Agent  
**Scope:** Complete application user interface analysis

---

## 1. App Structure Diagram

### Overall Architecture

```
Handy Application
├── Onboarding Flow (First-time user experience)
│   ├── AccessibilityOnboarding (permissions)
│   └── Onboarding (model selection)
│
├── Main Application Window
│   ├── Sidebar Navigation (7 sections)
│   │   ├── General (HandyHand icon)
│   │   ├── Models (Cpu icon)
│   │   ├── Advanced (Cog icon)
│   │   ├── History (History icon)
│   │   ├── Post Process* (Sparkles icon) - conditional
│   │   ├── Debug* (FlaskConical icon) - conditional
│   │   └── About (Info icon)
│   │
│   ├── Main Content Area (settings panels)
│   └── Footer (update status, version info)
│
├── Tray Menu
│   ├── Settings...
│   ├── Check for Updates...
│   ├── Copy Last Transcript
│   ├── Unload Model
│   ├── Model (submenu)
│   ├── Cancel
│   └── Quit
│
└── Overlay (recording feedback)
    ├── Transcribing...
    ├── Processing...
    ├── USB cycling states
    └── Edit text interface
```

### Navigation Flow

```
User launches app
    ↓
[Onboarding Check] ──→ No models? ──→ AccessibilityOnboarding
    ↓                      ↓
Has models?           Permissions granted?
    ↓                      ↓
Main App              Onboarding (Model selection)
    ↓                      ↓
Sidebar sections      Model downloaded
    ↓                      ↓
Settings panels ←─── Main App opens
```

---

## 2. User-Facing Text by Screen/Tab

### 2.1 Onboarding Flow

#### AccessibilityOnboarding Screen

| Text Element                                      | Label                                                       | Location      | Assessment                                   |
| ------------------------------------------------- | ----------------------------------------------------------- | ------------- | -------------------------------------------- |
| Logo                                              | HandyTextLogo (visual)                                      | Top center    | ✅ Clear brand identifier                    |
| Title                                             | "Permissions Required"                                      | Center header | ✅ Clear, action-oriented                    |
| Description                                       | "Handy needs a couple of permissions to work properly."     | Below title   | ⚠️ Vague - doesn't specify which permissions |
| **Microphone Permission**                         |                                                             |               |                                              |
| Title                                             | "Microphone Access"                                         | Card header   | ✅ Clear                                     |
| Description                                       | "Required to hear your voice for transcription."            | Card body     | ✅ Clear purpose                             |
| Button (idle)                                     | "Grant Permission"                                          | Card action   | ✅ Action-oriented                           |
| Button (Windows)                                  | "Open System Settings"                                      | Card action   | ⚠️ Different label on Windows - may confuse  |
| Status (granted)                                  | "Granted" with checkmark                                    | Card status   | ✅ Clear confirmation                        |
| Status (waiting)                                  | "Waiting..." with spinner                                   | Card status   | ✅ Clear loading state                       |
| **Accessibility Permission**                      |                                                             |               |                                              |
| Title                                             | "Accessibility Access"                                      | Card header   | ⚠️ Technical term - macOS-specific           |
| Description                                       | "Required to type transcribed text into your applications." | Card body     | ✅ Clear purpose                             |
| Button                                            | "Grant Permission"                                          | Card action   | ✅ Clear                                     |
| Success State                                     |                                                             |               |                                              |
| Title                                             | "All set!"                                                  | Center        | ✅ Friendly confirmation                     |
| Error Messages                                    |                                                             |               |                                              |
| "Failed to check permissions. Please try again."  | Toast                                                       | Error state   | ⚠️ Generic - no recovery guidance            |
| "Failed to request permission. Please try again." | Toast                                                       | Error state   | ⚠️ Generic                                   |

**UX Concern:** The "Accessibility Access" label uses macOS terminology that may confuse Windows/Linux users. The permission description doesn't explain WHY accessibility is needed (keyboard simulation).

---

#### Onboarding (Model Selection) Screen

| Text Element                   | Label                                          | Location           | Assessment                                                     |
| ------------------------------ | ---------------------------------------------- | ------------------ | -------------------------------------------------------------- |
| Subtitle                       | "To get started, choose a transcription model" | Below logo         | ✅ Clear call-to-action                                        |
| **Model Cards**                |                                                |                    |                                                                |
| "Recommended"                  | Badge                                          | Featured models    | ✅ Clear guidance                                              |
| "Active"                       | Badge                                          | Selected model     | ✅ Clear status                                                |
| "Custom"                       | Badge                                          | Custom models      | ⚠️ May confuse - what does "custom" mean?                      |
| "Switching..."                 | Badge                                          | Model loading      | ✅ Clear progress                                              |
| **Download States**            |                                                |                    |                                                                |
| "Downloading {{percentage}}%"  | Progress text                                  | Downloading model  | ✅ Clear progress                                              |
| "{{speed}} MB/s"               | Speed text                                     | Downloading model  | ✅ Technical but helpful                                       |
| "Verifying..."                 | Status                                         | Verifying download | ✅ Clear                                                       |
| "Extracting..."                | Status                                         | Extracting model   | ✅ Clear                                                       |
| "Cancel"                       | Button                                         | During download    | ✅ Clear                                                       |
| **Model Names & Descriptions** |                                                |                    |                                                                |
| Model names vary               | e.g., "Whisper Small", "Parakeet V2", etc.     | Card headers       | ⚠️ Technical model names - descriptions help but limited space |
| Accuracy/Speed bars            | "accuracy", "speed"                            | Model metadata     | ✅ Visual indicators with labels                               |
| Language support               | "Multi-language", "English Only"               | Model capabilities | ✅ Clear                                                       |
| "Translate to English"         | Capability                                     | Translation models | ✅ Clear                                                       |

**Missing Tooltips:**

- No tooltip explaining "Custom" badge
- No tooltip explaining accuracy/speed scoring methodology
- No explanation of what "Parakeet", "Whisper", etc. mean to non-technical users

---

### 2.2 Main Application - Sidebar Navigation

| Section        | Label (en)     | Icon         | Assessment                                  |
| -------------- | -------------- | ------------ | ------------------------------------------- |
| general        | "General"      | HandyHand    | ✅ Clear, default section                   |
| models         | "Models"       | Cpu          | ✅ Clear                                    |
| advanced       | "Advanced"     | Cog          | ✅ Clear                                    |
| history        | "History"      | History      | ✅ Clear                                    |
| postprocessing | "Post Process" | Sparkles     | ⚠️ Shortened - full name "Post Processing"? |
| debug          | "Debug"        | FlaskConical | ✅ Clear for technical users                |
| about          | "About"        | Info         | ✅ Standard                                 |

**Conditional Sections:**

- "Post Process" only appears when `post_process_enabled` is true
- "Debug" only appears when `debug_mode` is true

**Debug Hint (Sidebar Footer):**
| Text | "Debug settings:" ⌘⇧D (or Ctrl+Shift+D) | Bottom of sidebar | ✅ Helpful hint for power users |

---

### 2.3 General Settings Tab

#### Group: "General"

| Setting                                | Title                 | Description                                                                                                                                                           | Assessment                                    |
| -------------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| ShortcutInput (transcribe)             | "Transcribe Shortcut" | "The keyboard shortcut to record and transcribe your voice."                                                                                                          | ✅ Clear                                      |
| ShortcutInput (cancel)                 | "Cancel Shortcut"     | "The keyboard shortcut to cancel the current recording."                                                                                                              | ✅ Clear                                      |
| PushToTalk                             | "Push To Talk"        | "Hold to record, release to stop"                                                                                                                                     | ✅ Clear                                      |
| ShortcutInput (transcribe_with_router) | "Router Hotkey"       | "Records speech, transcribes it, and sends the text to your router system for classification and filing. No text is pasted — you'll get a notification via Telegram." | ⚠️ Technical jargon - "router system" unclear |

**UX Concern:** "Router Hotkey" description uses internal terminology ("router system") that won't make sense to users. The Telegram notification detail is also confusing.

#### Group: "Sound"

| Setting              | Title                  | Description                                                     | Assessment |
| -------------------- | ---------------------- | --------------------------------------------------------------- | ---------- |
| MicrophoneSelector   | "Microphone"           | "Select your preferred microphone device"                       | ✅ Clear   |
| MuteWhileRecording   | "Mute While Recording" | "Mute system audio during recording"                            | ✅ Clear   |
| AudioFeedback        | "Audio Feedback"       | "Play sound when recording starts and stops"                    | ✅ Clear   |
| OutputDeviceSelector | "Output Device"        | "Select your preferred audio output device for feedback sounds" | ✅ Clear   |
| VolumeSlider         | "Volume"               | "Adjust the volume of audio feedback sounds"                    | ✅ Clear   |

---

### 2.4 Models Settings Tab

| Element                          | Text                                                                                                                       | Assessment |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------- | ---------- |
| Title                            | "Transcription Models"                                                                                                     | ✅ Clear   |
| Description                      | "Select a transcription model or download additional models. Different models offer varying levels of accuracy and speed." | ✅ Clear   |
| Section: "Your Models"           | Downloaded models header                                                                                                   | ✅ Clear   |
| Section: "Available to Download" | Available models header                                                                                                    | ✅ Clear   |
| Filter: "All Languages"          | Language filter dropdown                                                                                                   | ✅ Clear   |

#### Benchmark Section

| Element               | Text                                                                                                                | Assessment                                      |
| --------------------- | ------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| Title                 | "Benchmark Models"                                                                                                  | ⚠️ Technical term - what does "benchmark" mean? |
| Description           | "Run a speed benchmark using your recorded audio clips. This measures actual transcription speed on your hardware." | ✅ Clear explanation                            |
| Button                | "Run Benchmark" / "Benchmarking..."                                                                                 | ✅ Clear                                        |
| Status                | "{{count}} audio clips available for benchmark"                                                                     | ✅ Informative                                  |
| Status (insufficient) | "You need at least {{needed}} audio clips..."                                                                       | ✅ Clear requirement                            |
| Result                | "✓ Benchmark complete!"                                                                                             | ✅ Clear success                                |
| Score display         | "Green bars show measured speed from your latest benchmark"                                                         | ✅ Helpful context                              |

**UX Concern:** "Benchmark" is technical jargon. Consider "Test Speed" or "Performance Test" for non-technical users.

#### Model Card States

| State         | Label                         | Assessment                    |
| ------------- | ----------------------------- | ----------------------------- |
| Downloading   | "Downloading {{percentage}}%" | ✅ Clear                      |
| Verifying     | "Verifying..."                | ✅ Clear                      |
| Extracting    | "Extracting..."               | ✅ Clear                      |
| Active        | "Active" with checkmark       | ✅ Clear                      |
| Available     | No label (clickable)          | ⚠️ Not obvious it's clickable |
| Hybrid badges | "SHORT", "LONG"               | ⚠️ Unclear without context    |

---

### 2.5 Advanced Settings Tab

#### Group: "App"

| Setting            | Title                   | Description                                                                                           | Assessment                                                       |
| ------------------ | ----------------------- | ----------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| StartHidden        | "Start Hidden"          | "Launch to system tray without opening the window."                                                   | ✅ Clear                                                         |
| AutostartToggle    | "Launch on Startup"     | "Automatically start Handy when you log in to your computer."                                         | ✅ Clear                                                         |
| ShowTrayIcon       | "Show Tray Icon"        | "Display the Handy icon in the system tray."                                                          | ✅ Clear                                                         |
| ShowOverlay        | "Overlay Position"      | "Display visual feedback overlay during recording and transcription. On Linux 'None' is recommended." | ⚠️ "Overlay" is technical; options: "None", "Bottom", "Top"      |
| ModelUnloadTimeout | "Unload Model"          | "Automatically free GPU/CPU memory when the model hasn't been used for the specified time"            | ⚠️ Technical - options like "After 2 minutes", "After 5 minutes" |
| ExperimentalToggle | "Experimental Features" | "Enable experimental features that are still in development."                                         | ✅ Clear warning                                                 |

#### Group: "Output"

| Setting           | Title                | Description                                                                                                                                                                | Assessment                                                                        |
| ----------------- | -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| PasteMethod       | "Paste Method"       | "Choose how text is inserted. Direct: simulates typing via system input. None: skips paste, only updates history/clipboard."                                               | ⚠️ Technical - "system input", options like "Clipboard (Cmd+V)", "Direct", "None" |
| TypingTool        | "Typing Tool"        | "Choose which Linux typing tool to use for Direct paste method. Auto will automatically detect and use the best available tool for your system."                           | ⚠️ Linux-specific in general settings                                             |
| ClipboardHandling | "Clipboard Handling" | "Don't Modify Clipboard preserves your current clipboard contents after transcription. Copy to Clipboard leaves the transcription result in your clipboard after pasting." | ⚠️ Double negative - confusing                                                    |
| AutoSubmit        | "Auto Submit"        | "Automatically send the selected key combination after text insertion. Cmd+Enter applies on macOS, while Windows/Linux use Super+Enter."                                   | ⚠️ "Super+Enter" may confuse Windows users                                        |

**UX Concern:** Clipboard handling description uses double negative ("Don't Modify"). The options are confusing:

- "Don't Modify Clipboard"
- "Copy to Clipboard"

Users may think "Don't Modify" means don't copy to clipboard at all, but it actually means preserve existing clipboard.

#### Group: "Transcription"

| Setting                 | Title                          | Description                                                                                                                                                       | Assessment                              |
| ----------------------- | ------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| WordCorrectionMode      | "Word Correction Mode"         | "Choose how custom words are matched: Fuzzy (word bias), Pronunciation (variants), or Exact (replacements)"                                                       | ⚠️ Technical jargon overload            |
| CustomWords             | "Custom Words"                 | "Add words that are often misheard or misspelled during transcription. The system will automatically correct similar-sounding words to match your list."          | ✅ Clear                                |
| CustomFillerWords       | "Custom Filler Words"          | "Add filler words to remove from transcriptions (like 'um', 'uh', 'like'). These words will be automatically filtered out from your transcribed text."            | ✅ Clear with examples                  |
| WordCorrectionThreshold | "Word Correction Threshold"    | "Sensitivity for custom word corrections"                                                                                                                         | ⚠️ Vague - what does threshold mean?    |
| AppendTrailingSpace     | "Append Trailing Space"        | "Add a space after pasted transcription"                                                                                                                          | ✅ Clear                                |
| ConvertUsToBritish      | "Convert to British English"   | "Convert US English spelling to British English (e.g., color → colour, analyze → analyse, center → centre)."                                                      | ✅ Clear with examples                  |
| HybridMode              | "Hybrid Mode"                  | "Automatically pick the best model based on audio length. Short recordings use a fast, hallucination-resistant model; long recordings use a more accurate model." | ⚠️ "Hallucination" is AI jargon         |
| AdaptiveThresholds      | "Adaptive Parakeet Thresholds" | "Automatically adjust Parakeet's internal thresholds based on audio characteristics for optimal transcription quality."                                           | ⚠️ Technical - "Parakeet", "thresholds" |
| VerificationMode        | "Verification Mode"            | "Enable two-pass transcription. After the first transcription, a second model verifies and corrects uncertain segments for improved accuracy."                    | ✅ Clear                                |
| VadSensitivity          | "Voice Detection Sensitivity"  | "Adjust how sensitive the voice activity detection is. Higher sensitivity detects speech more aggressively, while lower sensitivity requires clearer speech."     | ✅ Clear                                |
| LiveCaptions            | "Live Captions"                | "Show live captions below the volume bars during recording. Uses additional CPU."                                                                                 | ⚠️ "Volume bars" reference unclear      |

**Hybrid Mode Sub-settings:**

| Setting    | Title                                                         | Description                                                                                   | Assessment           |
| ---------- | ------------------------------------------------------------- | --------------------------------------------------------------------------------------------- | -------------------- |
| Threshold  | "Length Threshold"                                            | "Audio shorter than this uses the short audio model; longer audio uses the long audio model." | ✅ Clear             |
| AssignHint | "Assign models as SHORT or LONG in the Models section above." | Helper text                                                                                   | ✅ Clear instruction |

#### Group: "History"

| Setting            | Title                    | Description                                         | Assessment |
| ------------------ | ------------------------ | --------------------------------------------------- | ---------- |
| HistoryLimit       | "History Limit"          | "Maximum number of history entries to keep"         | ✅ Clear   |
| RecordingRetention | "Auto-Delete Recordings" | "Automatically delete old recordings to save space" | ✅ Clear   |

**Options:**

- "Never"
- "Keep latest {{count}}"
- "After 3 days"
- "After 2 weeks"
- "After 3 months"

#### Group: "Experimental" (conditional)

| Setting                | Title                                                     | Description                                                                                                                                                                  | Assessment                      |
| ---------------------- | --------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------- |
| PostProcessingToggle   | "Post Processing"                                         | "Enable AI-powered text refinement after transcription"                                                                                                                      | ✅ Clear                        |
| KeyboardImplementation | "Keyboard Implementation"                                 | "Choose the keyboard shortcut backend."                                                                                                                                      | ⚠️ Technical - "backend" jargon |
| AccelerationSelector   | "Whisper Acceleration", "ONNX Acceleration", "GPU Device" | Hardware acceleration options                                                                                                                                                | ⚠️ Technical - "ONNX", "GPU"    |
| LazyStreamClose        | "Keep Mic Open Between Transcriptions"                    | "Keeps the microphone stream open for 30 seconds after recording stops, reducing latency for back-to-back transcriptions. May degrade Bluetooth audio quality while active." | ✅ Clear with trade-off noted   |

---

### 2.6 Post Process Settings Tab (conditional)

#### Group: "Hotkey"

| Setting       | Title                    | Description                                                                                  | Assessment |
| ------------- | ------------------------ | -------------------------------------------------------------------------------------------- | ---------- |
| ShortcutInput | "Post-Processing Hotkey" | "Optional: A dedicated hotkey that always applies AI post-processing to your transcription." | ✅ Clear   |

#### Group: "API (OpenAI Compatible)"

| Setting            | Title                | Description                                                                       | Assessment                               |
| ------------------ | -------------------- | --------------------------------------------------------------------------------- | ---------------------------------------- |
| Provider           | "Provider"           | "Select an OpenAI-compatible provider."                                           | ⚠️ "OpenAI-compatible" assumes knowledge |
| Apple Intelligence | "Apple Intelligence" | "Runs fully on-device. No API key or network access is required."                 | ✅ Clear                                 |
| Base URL           | "Base URL"           | "API base URL for the selected provider. Only the custom provider can be edited." | ⚠️ Technical                             |
| API Key            | "API Key"            | "API key for the selected provider."                                              | ⚠️ Vague - where do users get this?      |
| Model              | "Model"              | "Choose a model exposed by the selected provider."                                | ⚠️ "Exposed" is technical                |

**Apple Intelligence Requirements:**
| Text | "Requires an Apple Silicon Mac running macOS Tahoe (26.0) or later. Apple Intelligence must be enabled in System Settings." | Requirements description | ⚠️ "Tahoe" is internal codename - users know it as macOS 15+ |

#### Group: "Prompt"

| Setting             | Title                                                               | Description                                                                                                                                     | Assessment                       |
| ------------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------- | -------- |
| Selected Prompt     | "Selected Prompt"                                                   | "Select a template for refining transcriptions or create a new one. Use ${output} inside the prompt text to reference the captured transcript." | ⚠️ Template language may confuse |
| Create New          | "Create New Prompt"                                                 | Button                                                                                                                                          | ✅ Clear                         |
| Prompt Label        | "Prompt Label"                                                      | "Enter prompt name"                                                                                                                             | Placeholder                      | ✅ Clear |
| Prompt Instructions | "Prompt Instructions"                                               | "Write the instructions to run after transcription."                                                                                            | ✅ Clear                         |
| Tip                 | "Tip: Use ${output} to insert the transcribed text in your prompt." | Helper text                                                                                                                                     | ✅ Clear                         |

**Missing Context:**

- No examples of what post-processing prompts can do
- No explanation of what "AI-powered text refinement" actually means
- No cost warning for API usage

---

### 2.7 History Settings Tab

| Element    | Text                                                            | Assessment     |
| ---------- | --------------------------------------------------------------- | -------------- | -------------------- |
| Title      | "History"                                                       | Section header | ✅ Clear             |
| Button     | "Open Recordings Folder"                                        | Action         | ✅ Clear             |
| Search     | "Search history..."                                             | Placeholder    | ✅ Clear             |
| Loading    | "Loading history..."                                            | Status         | ✅ Clear             |
| Empty      | "No transcriptions yet. Start recording to build your history!" | Empty state    | ✅ Friendly          |
| Error      | "Failed to load history"                                        | Error          | ⚠️ No retry guidance |
| No Results | "No matching transcriptions found"                              | Search empty   | ✅ Clear             |

#### History Entry Actions

| Action       | Tooltip/Label                               | Assessment                |
| ------------ | ------------------------------------------- | ------------------------- | ---------- |
| Copy         | "Copy transcription to clipboard"           | ✅ Clear                  |
| Save/Unsave  | "Save transcription" / "Remove from saved"  | ✅ Clear                  |
| Retranscribe | "Re-transcribe"                             | ⚠️ Unclear what this does |
| Delete       | "Delete entry"                              | ✅ Clear                  |
| Delete Error | "Failed to delete entry. Please try again." | Error                     | ⚠️ Generic |

#### Saved Entry Metadata (appears when entry is saved)

| Element      | Text                                           | Assessment                                    |
| ------------ | ---------------------------------------------- | --------------------------------------------- |
| Ground Truth | "Ground Truth: {{text}}"                       | ⚠️ Technical term - "what you actually said"? |
| Quality      | "Quality:" with options "Good", "Okay", "Bad"  | ✅ Clear                                      |
| Speed        | "Speed:" with options "Fast", "Normal", "Slow" | ✅ Clear                                      |

**UX Concern:** "Ground Truth" is machine learning terminology. Users won't understand this term. "What did you actually say?" would be clearer.

---

### 2.8 Debug Settings Tab (conditional)

#### Group: "Debug"

| Setting             | Title                  | Description                                                                                       | Assessment                            |
| ------------------- | ---------------------- | ------------------------------------------------------------------------------------------------- | ------------------------------------- |
| LogLevel            | "Log Level"            | "Set the verbosity of logging"                                                                    | ⚠️ Technical - "verbosity", "logging" |
| UpdateChecks        | "Check for Updates"    | "Automatically check for new versions of Handy"                                                   | ✅ Clear                              |
| SoundTheme          | "Sound Theme"          | "Choose a sound theme for recording start and stop feedback"                                      | ✅ Clear                              |
| PasteDelay          | "Paste Delay"          | "Delay before sending paste keystroke (in milliseconds). Increase if wrong text is being pasted." | ✅ Clear                              |
| RecordingBuffer     | "Recording Buffer"     | "Maximum extra time (ms) to keep recording after releasing the key..."                            | ⚠️ Long and technical                 |
| PreRecordingBuffer  | "Pre-Recording Buffer" | "Capture audio from before you pressed the hotkey..."                                             | ⚠️ Very long description              |
| AlwaysOnMicrophone  | "Always-On Microphone" | "Keep microphone active for faster response"                                                      | ✅ Clear                              |
| ClamshellMicrophone | "Clamshell Microphone" | "Microphone to use when laptop lid is closed"                                                     | ⚠️ "Clamshell" is jargon              |
| UsbWatchdog         | "USB Power Watchdog"   | "Automatically power-cycle the USB port when the microphone can't be opened."                     | ⚠️ Technical jargon                   |

#### Repetition Suppression Section

| Element                | Text                                                                                    | Assessment                |
| ---------------------- | --------------------------------------------------------------------------------------- | ------------------------- | ---------- |
| Title                  | "Repetition Suppression"                                                                | ⚠️ Technical              |
| Description            | "Remove repeated consecutive words from transcriptions..."                              | ✅ Clear with explanation |
| Protected Words Notice | "Protected words: I, you, the, a, very, so, and, or, but, to, for, with, is, was, etc." | Helper                    | ✅ Helpful |

---

### 2.9 About Settings Tab

| Setting             | Title                  | Description                                  | Assessment |
| ------------------- | ---------------------- | -------------------------------------------- | ---------- |
| AppLanguage         | "Application Language" | "Change the language of the Handy interface" | ✅ Clear   |
| Version             | "Version"              | "Current version of Handy"                   | ✅ Clear   |
| Source Code         | "Source Code"          | "View source code and contribute"            | ✅ Clear   |
| Support Development | "Support Development"  | "Help us continue building Handy"            | ✅ Clear   |
| AppDataDirectory    | "App Data Directory"   | "Location where Handy stores its data"       | ✅ Clear   |
| LogDirectory        | "Log Directory"        | "Location where log files are stored"        | ✅ Clear   |

#### Acknowledgments

| Element     | Text                                                                                | Assessment  |
| ----------- | ----------------------------------------------------------------------------------- | ----------- | ------------ |
| Title       | "Acknowledgments"                                                                   | Section     | ✅ Clear     |
| Whisper.cpp | "High-performance inference of OpenAI's Whisper automatic speech recognition model" | Description | ⚠️ Technical |

---

### 2.10 Overlay UI

| State       | Text                                                                                 | Assessment                         |
| ----------- | ------------------------------------------------------------------------------------ | ---------------------------------- | ------------ |
| Recording   | "Transcribing..."                                                                    | ✅ Clear                           |
| Processing  | "Processing..."                                                                      | ⚠️ Vague - what's being processed? |
| USB Cycling | "USB cycling…", "Locating USB device…", "Power cycling port…", "Waiting for device…" | Status sequence                    | ⚠️ Technical |
| USB Success | "Audio recovered"                                                                    | ✅ Clear                           |
| Hybrid Mode | "S", "L" badges                                                                      | ⚠️ Unclear without legend          |
| Routing     | "Routing…", "Filing…"                                                                | ⚠️ Technical/internal terms        |
| Error       | "Microphone not responding", "Low audio - check microphone"                          | ✅ Clear                           |
| Confirm     | "Sending in", countdown                                                              | ✅ Clear                           |
| Edit Mode   | "Edit text:", "Send", "Cancel"                                                       | ✅ Clear                           |
| No Speech   | "No speech detected"                                                                 | ✅ Clear                           |

---

### 2.11 Error Messages (Toasts)

| Error             | Text                                                                             | Assessment                      |
| ----------------- | -------------------------------------------------------------------------------- | ------------------------------- |
| Mic Permission    | "Microphone Access Denied" + platform-specific instructions                      | ✅ Clear with platform guidance |
| No Input Device   | "No Microphone Found" + "No audio input device was detected..."                  | ✅ Clear                        |
| Recording Failed  | "Failed to start recording: {{error}}"                                           | ⚠️ Technical error detail       |
| Model Load Failed | "Failed to load model: {{model}}"                                                | ⚠️ Vague - why failed?          |
| Paste Failed      | "Failed to Paste Text" + "Text could not be pasted into the active application." | ✅ Clear                        |

---

### 2.12 Tray Menu

| Item          | Label                  | Assessment                        |
| ------------- | ---------------------- | --------------------------------- |
| Settings      | "Settings..."          | ✅ Clear                          |
| Check Updates | "Check for Updates..." | ✅ Clear                          |
| Copy Last     | "Copy Last Transcript" | ✅ Clear                          |
| Unload Model  | "Unload Model"         | ⚠️ Technical - what does this do? |
| Model         | "Model" (submenu)      | ✅ Clear                          |
| Cancel        | "Cancel"               | ✅ Clear                          |
| Quit          | "Quit"                 | ✅ Clear                          |

---

## 3. Tooltip/Hint Coverage Assessment

### Settings with Tooltips (Good Coverage)

Most settings in the app use `descriptionMode="tooltip"` which provides helpful context on hover. The Tooltip component displays a question mark icon that shows the description on hover/click.

**Well-covered areas:**

- General settings (shortcuts, audio)
- Advanced settings (most toggles)
- Post-processing settings
- Debug settings

### Settings Missing or with Weak Tooltips

| Setting                  | Issue                     | Recommendation                             |
| ------------------------ | ------------------------- | ------------------------------------------ |
| **Word Correction Mode** | Complex technical concept | Add visual examples of each mode           |
| **Hybrid Mode**          | "Hallucination" jargon    | Replace with "errors" or "mistakes"        |
| **Benchmark**            | Technical term            | Add explanation of what benchmarking means |
| **Router Hotkey**        | Internal terminology      | Rewrite description for users              |
| **USB Watchdog**         | Technical jargon          | Explain in plain language                  |
| **Clamshell Microphone** | Technical term            | "Laptop closed microphone"                 |
| **Ground Truth**         | ML terminology            | "What you actually said"                   |
| **Paste Method**         | Technical options         | Add visual examples of each method         |
| **Clipboard Handling**   | Double negative           | Rewrite for clarity                        |

---

## 4. UX Concerns and Recommendations

### 4.1 High Priority Issues

#### Issue: Technical Jargon Overload

**Problem:** Many settings use technical terms that non-technical users won't understand.

**Examples:**

- "Hallucination" (AI-specific)
- "Ground Truth" (ML-specific)
- "Parakeet" (model name without context)
- "Router" (internal system name)
- "ONNX", "GPU Device", "Backend"
- "Clamshell" (laptop mode)
- "USB Power Watchdog"

**Recommendations:**

1. Replace jargon with plain language
2. Add a "Learn more" link for complex features
3. Provide examples for technical concepts
4. Consider a "Simple/Advanced" mode toggle

---

#### Issue: Inconsistent Naming

**Examples:**

- "Post Process" (sidebar) vs "Post Processing" (settings)
- "Transcribe Shortcut" vs "Router Hotkey" vs "Post-Processing Hotkey" (inconsistent suffixes)
- "Debug" section uses `settings.debug.*` i18n keys but some labels are under `settings.advanced.*`

**Recommendations:**

1. Standardize on "Post Processing"
2. Use consistent naming patterns: "[Action] Shortcut" or "[Action] Hotkey"
3. Review i18n key organization

---

#### Issue: Confusing Clipboard Handling

**Current Options:**

- "Don't Modify Clipboard"
- "Copy to Clipboard"

**Problem:** The double negative is confusing. Users may think "Don't Modify" means the clipboard won't contain the transcription.

**Recommendation:**

```
Current:  "Don't Modify Clipboard" / "Copy to Clipboard"
Better:    "Keep original clipboard" / "Replace with transcription"
Or:        "Preserve existing clipboard" / "Store transcription in clipboard"
```

---

#### Issue: Missing Context for Complex Features

**Problem:** Features like Post Processing and Hybrid Mode lack sufficient explanation.

**Current:**

- "Enable AI-powered text refinement after transcription"
- "Automatically pick the best model based on audio length"

**Missing:**

- What does "text refinement" actually do?
- Examples of before/after
- Cost implications (for API-based features)
- Visual explanation of hybrid mode

**Recommendations:**

1. Add inline examples
2. Link to documentation
3. Show preview/demo
4. Add cost warnings for API features

---

### 4.2 Medium Priority Issues

#### Issue: Hidden Sections Without Explanation

**Problem:** Debug and Post Processing sections appear/disappear based on settings, with no explanation.

**Recommendation:**

1. Show disabled sections with explanation: "Enable experimental features to access Debug settings"
2. Or show a hint: "Press ⌘⇧D to enable Debug mode"

---

#### Issue: Accessibility Permission Description

**Current:** "Required to type transcribed text into your applications."

**Problem:** Doesn't explain WHY accessibility is needed (keyboard simulation).

**Better:** "Required to automatically type transcribed text into any application. This permission allows Handy to simulate keyboard input."

---

#### Issue: Model Selection for Non-Technical Users

**Problem:** Model names (Whisper, Parakeet, Moonshine, Canary) mean nothing to average users.

**Current descriptions:**

- "Fast and fairly accurate"
- "English only. The best model for English speakers."

**Recommendations:**

1. Add recommended use cases: "Best for dictation", "Best for meetings", etc.
2. Show language support more prominently
3. Consider hiding model names behind user-friendly labels

---

### 4.3 Low Priority Issues

#### Issue: Inconsistent Button Labels Across Platforms

**Problem:** "Grant Permission" vs "Open System Settings" for the same action on different platforms.

**Recommendation:** Consider platform-specific i18n keys that explain the difference.

---

#### Issue: Error Messages Lack Recovery Guidance

**Examples:**

- "Failed to check permissions. Please try again."
- "Failed to load history"

**Recommendation:** Add actionable next steps:

- "Failed to check permissions. Please try again or restart Handy."
- "Failed to load history. Check your internet connection or try again."

---

### 4.4 Positive Findings

#### Excellent Elements:

1. **Comprehensive i18n** - All text is internationalized
2. **Consistent tooltip pattern** - Question mark icon is clear
3. **Good empty states** - Friendly messages like "No transcriptions yet. Start recording to build your history!"
4. **Visual feedback** - Loading states, progress bars, badges
5. **Helpful hints** - Debug mode shortcut hint in sidebar
6. **Platform-aware** - Different instructions for macOS/Windows/Linux

#### Well-Designed Features:

1. **Push to Talk toggle** - Simple on/off with clear description
2. **Custom filler words** - Good examples provided ("um", "uh", "like")
3. **British English conversion** - Clear examples (color → colour)
4. **Hybrid mode** - Good visual hierarchy with expandable options

---

## 5. Summary Table: Label Clarity Assessment

| Clarity Level        | Count | Examples                                                                                           |
| -------------------- | ----- | -------------------------------------------------------------------------------------------------- |
| ✅ Clear             | ~65%  | "Microphone", "Launch on Startup", "Copy to Clipboard"                                             |
| ⚠️ Needs Improvement | ~25%  | "Ground Truth", "Hallucination", "Router Hotkey", "USB Watchdog"                                   |
| ❌ Confusing         | ~10%  | "Don't Modify Clipboard", "Accessibility Access" (on Windows), "Post Process" vs "Post Processing" |

---

## 6. Recommendations Summary

### Immediate Actions

1. **Fix inconsistent naming:** "Post Process" → "Post Processing" in sidebar
2. **Rewrite Clipboard Handling options** to remove double negative
3. **Add "Ground Truth" explanation** or rename to "What you actually said"
4. **Fix Router Hotkey description** to remove internal terminology

### Short-term Improvements

1. **Add visual examples** for complex features (Word Correction Mode, Paste Method)
2. **Create a glossary** or inline explanations for technical terms
3. **Add cost warnings** for API-based features
4. **Improve error messages** with recovery steps

### Long-term Considerations

1. **Consider Simple/Advanced mode** to hide technical settings
2. **Redesign model selection** with user-friendly labels
3. **Add onboarding tooltips** for first-time users
4. **Create feature tours** for complex functionality

---

## Appendix: Complete Text Inventory by Section

### A. Sidebar Navigation

```
sidebar.general: "General"
sidebar.models: "Models"
sidebar.advanced: "Advanced"
sidebar.postProcessing: "Post Process"
sidebar.history: "History"
sidebar.debug: "Debug"
sidebar.about: "About"
sidebar.debugHint: "Debug settings:"
```

### B. Onboarding

```
onboarding.subtitle: "To get started, choose a transcription model"
onboarding.recommended: "Recommended"
onboarding.permissions.title: "Permissions Required"
onboarding.permissions.description: "Handy needs a couple of permissions to work properly."
onboarding.permissions.microphone.title: "Microphone Access"
onboarding.permissions.microphone.description: "Required to hear your voice for transcription."
onboarding.permissions.accessibility.title: "Accessibility Access"
onboarding.permissions.accessibility.description: "Required to type transcribed text into your applications."
onboarding.permissions.grant: "Grant Permission"
onboarding.permissions.granted: "Granted"
onboarding.permissions.waiting: "Waiting..."
onboarding.permissions.allGranted: "All set!"
```

### C. Model Names

```
Whisper Small: "Fast and fairly accurate."
Whisper Medium: "Good accuracy, medium speed"
Whisper Turbo: "Balanced accuracy and speed."
Whisper Large: "Good accuracy, but slow."
Parakeet V2: "English only. The best model for English speakers."
Parakeet V3: "Fast and accurate"
Moonshine Base: "Very fast, English only. Handles accents well."
SenseVoice: "Very fast. Chinese, English, Japanese, Korean, Cantonese."
[Plus 8 more models...]
```

### D. Settings Groups

```
settings.general.title: "General"
settings.sound.title: "Sound"
settings.models.title: "Transcription Models"
settings.advanced.title: "Advanced"
settings.advanced.groups.app: "App"
settings.advanced.groups.output: "Output"
settings.advanced.groups.transcription: "Transcription"
settings.advanced.groups.history: "History"
settings.advanced.groups.experimental: "Experimental"
settings.postProcessing.title: "Post Process"
settings.history.title: "History"
settings.debug.title: "Debug"
settings.about.title: "About"
```

### E. Common Actions

```
common.loading: "Loading..."
common.save: "Save"
common.cancel: "Cancel"
common.reset: "Reset"
common.add: "Add"
common.remove: "Remove"
common.delete: "Delete"
common.edit: "Edit"
common.create: "Create"
common.update: "Update"
common.close: "Close"
common.open: "Open"
common.default: "Default"
common.enabled: "Enabled"
common.disabled: "Disabled"
```

---

_Report generated for Handy-Mac UI/UX review. All text extracted from `/src/i18n/locales/en/translation.json` and component analysis._
