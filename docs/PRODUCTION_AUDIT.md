# VeeType production audit and implemented recovery pass

Date: 2026-10-11. Release baseline: `10c908a`, v0.3.8. The supplied directive ends at Part 1; no subsequent phase specifications were included. This document records the inspected architecture, completed targeted changes, actual validation, and outstanding work. It does not certify the whole application as production-ready.

## Repository and actual architecture

The working tree was clean before this pass. No AGENTS.md was found. Existing visual design, waveform, branding, native framework, licensing policy, account access, and entitlements were preserved.

The actual entry point is `src/main.rs::main` (lines 549-616). It selects desktop, settings, vault, or Parakeet worker mode. The desktop service owns a Win32 hotkey/message loop, per-dictation CPAL capture, adaptive local speech inference, optional text polishing, Windows Unicode input, and local history. Settings and Vault use eframe/egui windows; the overlay is native Win32/GDI. There is no Electron or Tauri runtime.

Module inventory:

| Area | Source and responsibilities |
| --- | --- |
| Lifecycle and routing | `src/main.rs`, `src/config.rs`, `src/utils/hotkey.rs` |
| Audio and speech | `src/engine/audio.rs`, `src/engine/parakeet.rs` |
| Scheduling and hardware | `src/engine/adaptive.rs`, `src/engine/hardware.rs` |
| Text processing | `src/engine/llm.rs`, `src/engine/cloud.rs`, `src/engine/text_cleanup.rs` |
| Windows integration | `src/text_input.rs`, `src/engine/startup.rs`, `src/engine/launcher.rs`, `src/ui/tray.rs` |
| UI and history | `src/ui/settings.rs`, `src/ui/overlay.rs`, `src/ui/vault.rs` |
| Assets and updates | `src/engine/downloader.rs`, `src/engine/updater.rs`, `build.rs`, `setup.iss`, `.github/workflows/release.yml` |
| Optional retained account infrastructure | `src/engine/license.rs`, `src/engine/keychain.rs`, `supabase/` functions and migration; policies unchanged |
| Product site | `index.html`; separate from the desktop capture/inference runtime |

Actual dependencies are in `Cargo.toml:6-52` and resolved in `Cargo.lock`: `cpal` 0.15.3, `whisper-rs` 0.16, `transcribe-cpp` 0.3.1, `llama-cpp-2`/sys pinned to 0.1.156, eframe 0.29, WebRTC VAD, nnnoiseless, windows-sys 0.52, keyring 2, and self_update 0.41. Neither enigo nor arboard is a dependency. Dictation uses `SendInput`, not clipboard paste.

## Findings and changes

Statuses describe the scope of evidence, not blanket guarantees.

| Classification | Finding and exact code evidence | Outcome |
| --- | --- | --- |
| VERIFIED WORKING | Published Parakeet CPU and Vulkan workers, `src/engine/parakeet.rs::run_worker`, lines 200-269; `scripts/Test-ParakeetWorker.ps1` | Official public speech sample recognized correctly twice per backend; same process/session reused; clean exit after input closes. |
| VERIFIED WORKING | Worker protocol, `src/engine/parakeet.rs::write_audio_request/read_response`, lines 271-323 | Unit tests cover exact PCM chunk boundaries, non-finite input rejection, Unicode responses, and oversized response rejection. |
| VERIFIED DEFECT, corrected locally | Pipe writes happened before the inference timeout, so a non-reading worker could block indefinitely; `ParakeetWorker::transcribe`, lines 130-158 | Dedicated bounded request channel and writer thread put transport inside the response deadline; child termination releases blocked writes. A forced native hang has not yet been injected. |
| VERIFIED DEFECT, corrected locally | Worker mode bypassed desktop priority setup; `run_worker`, lines 200-207 | Worker explicitly requests below-normal priority and launches without a console. Priority failure is still best-effort. |
| VERIFIED DEFECT, corrected locally | CPU retry used `?` and could terminate the desktop loop; `src/main.rs::run_app`, lines 1156-1205 | Retry failure feeds the existing recoverable recognition-error path. Next dictation can create a new worker. |
| VERIFIED DEFECT, corrected locally | Text insertion used the current window after inference, and insertion failure exited before Vault save; `run_app`, lines 1007-1009 and 1284-1322; `src/text_input.rs::send_text`, lines 22-69 | Original HWND/title captured before dictation processing. Transcript saved before insertion. Destination changes or denied input show a recoverable error. No duplicate retry of partially inserted text. |
| VERIFIED DEFECT, corrected locally | CPAL error callback only logged an error; `src/engine/audio.rs::AudioCapture::start/process_pending_samples`, lines 59-230 | Failure flag, five-second no-data detection, and 30-minute recording cap; `run_app` handles drain/finish failures without exiting. Failure-flag regression test passes; physical USB disconnect still needs testing. |
| VERIFIED DEFECT, corrected locally | Missing cloud credentials could prevent local dictation startup; `run_app`, lines 686-701 | Client initialization failure falls back to local processing. No cloud request was made in validation. |
| VERIFIED DEFECT, corrected locally | Recursive uninstall deleted the complete app folder; `setup.iss`, former `[UninstallDelete]` | Removed recursive deletion and added `uninsneveruninstall` to the config entry (`setup.iss:39`). Models, settings, history and logs are retained. Updated installer compile/uninstall validation is pending. |
| VERIFIED WORKING, unit scope | Audio timeline preservation and formatting/insertion batching, `src/engine/audio.rs` tests, lines 410-477; `src/text_input.rs` tests, lines 72-158 | Quiet edges, pauses, partial tail, Unicode, repeated characters, and failure-without-retry tests pass. This is not proof of live microphone-to-editor delivery. |
| PARTIALLY IMPLEMENTED | Live settings reload, `src/ui/settings.rs::save`, lines 264-291; `src/main.rs::run_app`, lines 892-966 | Separate config event and settings mutex exist. There is no applied-settings acknowledgement; UI success is optimistic. Hotkey failure rollback and four successive saves still require live testing. |
| PARTIALLY IMPLEMENTED | Model catalogue/import and file transcription, `src/ui/settings.rs::copy_model`, lines 1311-1338; `src/main.rs`, lines 840-873; `src/engine/audio.rs::transcribe_media_file`, lines 343-406 | Parakeet dictation is supported. Media-file transcription still requires Whisper and FFmpeg. Import checks extension, not full architecture/capability. |
| VERIFIED WORKING, release scope | `.github/workflows/release.yml::Calculate release version/Publish GitHub Release` | v0.3.8 tests, Vulkan compilation, ZIP, installer, and Release upload all succeeded; assets confirmed through GitHub API. |
| PARTIALLY IMPLEMENTED | Automatic version progression, `.github/workflows/release.yml:25-76` | Workflow creates a tag for the declared version; it does not automatically increment it. A cancelled run can leave a tag that suppresses publication on a rerun. Requires a dedicated retry/versioning change. |
| ARCHITECTURAL RISK | Blocking speech/polishing on the main message loop, `src/main.rs::run_app`, lines 1124-1282 | Hotkey/tray/settings events can wait during inference, despite the overlay's separate thread. Cancellation and asynchronous orchestration are not complete. |
| ARCHITECTURAL RISK | Whisper and llama.cpp remain in process, `src/main.rs::load_whisper_engine`, lines 375-420; `src/engine/llm.rs::CachedLlm/LocalLlm::polish` | Parakeet isolation does not protect the desktop from every other native engine crash. |
| ARCHITECTURAL RISK | Settings/history direct writes, `src/config.rs::save_config`, lines 231-235; `src/ui/vault.rs::save_entry`, lines 30-42 | Writes are not atomic; interrupted writes or concurrent Vault clear/save need testing and an atomic persistence design. |
| ARCHITECTURAL RISK | Capture resampling, `src/engine/audio.rs::AudioCapture::start`, lines 81-142 | Float-index sample selection lacks a proper anti-alias filter and should be measured at 44.1/48 kHz and long durations. |
| ARCHITECTURAL RISK | Update trust/coordination, `src/engine/updater.rs::perform_update`, lines 11-33 | HTTPS GitHub update path exists; signed-update verification and coordinated running-worker replacement are not established. |
| NOT IMPLEMENTED | Incremental Parakeet streaming in `src/engine/parakeet.rs::run_worker`, lines 232-267 | Uses batch `session.run` after recording. No claim of live partial text or competitor-equivalent streaming latency. |
| UNVERIFIED | Installed-app microphone/hotkey/editor path, CPU/GPU/RAM/power while idle and under load | No automated physical-microphone-to-Notepad/Word/browser test or power measurement was performed. No zero-impact or fastest-product claim. |

## Measurements actually taken

Official sample: [upstream JFK WAV](https://github.com/handy-computer/transcribe.cpp/blob/main/samples/jfk.wav), 11 seconds, mono 16 kHz PCM. No user audio or transcript was uploaded. Model SHA-256 matched the official publisher: `4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795`, 731,357,568 bytes.

| Executable | Backend / threads | Load | First request | Second request |
| --- | --- | --- | --- | --- |
| Published v0.3.8 ZIP | CPU / 2 | 732 ms | 10,038 ms | 9,940 ms |
| Published v0.3.8 ZIP | Vulkan / 2 | 627 ms | 1,905 ms | 182 ms |
| Hardened local debug build | CPU / 2 | 1,225 ms | 1,195 ms | 1,167 ms |

These are individual runs on this PC, not statistical benchmarks. The release CPU runtime has conservative instructions for compatibility; local native code is compiled differently. The timing difference warrants a future portable runtime-dispatched CPU build, with all runtime modules included in both installers and updates. There is no comparable competitor measurement.

All requests returned: “And so, my fellow Americans, ask not what your country can do for you. Ask what you can do for your country.” Both worker processes exited with code 0 after pipe closure. Local suite: `cargo test --locked`, 58 passed. `cargo build --locked` passed. Earlier standard Clippy completed with eight existing warnings; strict `-D warnings` did not pass. Do not label strict lint clean.

Published build: [GitHub Actions run 38148158792](https://github.com/narvindcad-sketch/VeeType/actions/runs/38148158792). [v0.3.8 release](https://github.com/narvindcad-sketch/VeeType/releases/tag/v0.3.8) contains `VeeType_Setup.exe` (11,940,207 bytes) and the OTA ZIP (19,472,645 bytes). Additional recovery changes described as “corrected locally” are not included in those assets.

## Remaining validation and follow-up sequence

1. Physical USB microphone capture, denial/reconnect, four repeated saves, hotkey changes, and insertion into standard editors; include foreground-window changes and elevated targets without bypassing Windows input restrictions.
2. Fault injection for non-reading/hung/crashed workers; bounded shutdown and retained-audio CPU retry. Test full supervisor behavior, not only the child protocol.
3. Atomic settings/history persistence and truthful settings-applied feedback; correct version/release retry automation.
4. Measure portable CPU dispatch, streaming, background memory retention, idle wakeups and power. Current retention is 900 seconds (`src/main.rs:789`); README previously incorrectly said two minutes.
5. Verify updated installer compilation and clean install/upgrade/uninstall in disposable Windows environments. Do not uninstall the user's installation to test retention.

External references are engineering evidence, not proprietary implementation claims: [Handy transcription source](https://github.com/cjpais/Handy/blob/main/src-tauri/src/managers/transcription.rs), [Handy dependencies](https://github.com/cjpais/Handy/blob/main/src-tauri/Cargo.toml), [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp), and [Inno Setup file flags](https://jrsoftware.org/ishelp/topic_filessection.htm). Full production sign-off remains outstanding.
