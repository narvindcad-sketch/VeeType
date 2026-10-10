# VeeType

VeeType is a Windows desktop voice-dictation application. It transcribes speech locally with Whisper, supports vocabulary hints and prompt modes, and can polish text with a local model or an optional Groq/OpenAI-compatible cloud provider.

The [`index.html`](index.html) file is a product landing page. Its interactive demo displays sample text only; it does not record audio or connect to the desktop application.

## Features

- Event-driven Win32 `WM_HOTKEY` dictation with no idle key polling, and a native, click-through floating pill with an animated indigo/violet waveform and processing indicator.
- Single-instance tray behavior; launching VeeType again opens Settings in the running instance.
- Text insertion does not use the system clipboard, so existing clipboard formats and contents are left untouched.
- Microphone capture preserves the full audio timeline, including quiet words and pauses. WebRTC VAD only controls hands-free silence detection; a 200 ms release tail and partial-frame flushing preserve word endings.
- Voice commands such as “new line,” “new paragraph,” “comma,” and “scratch that” apply formatting to polished dictation before insertion.
- Local Whisper transcription and optional English translation.
- Optional local GGUF text polishing or Groq/OpenAI cloud polishing.
- Custom vocabulary and writing prompts.
- Local searchable dictation history.
- Audio/video transcription through FFmpeg.

## Runtime behavior

VeeType is a regular desktop app when you open it: its control window appears so you can review settings, models, history, and updates. When **Start with Windows** is enabled, it instead starts quietly in the system tray and becomes visible only when you open it or use its tray menu.

The idle process is event-driven. It does not poll the keyboard, keep the microphone stream open, or animate the hidden overlay. Whisper and the optional local polishing model load only when a dictation needs them, stay warm briefly for follow-up dictation, and are released after two minutes of inactivity. The first dictation after launch or that idle timeout therefore takes a little longer while its model loads; later dictations are faster.

## Build

Build requirements: Windows, Rust/Cargo, and a working C++ build toolchain for native dependencies. A Whisper model is downloaded during first-run setup; model files are not required to compile the application.

```powershell
cargo test
cargo build --release
```

The release executable is `target\release\VeeType.exe`.
CPU inference is supported with up to four worker threads. To build the universal GPU-enabled release, install the LunarG Vulkan SDK and run `cargo build --release --features vulkan`. The Vulkan build can use supported NVIDIA, AMD, and Intel GPUs through their installed Vulkan drivers, with CPU inference as a fallback when GPU model initialization returns an error. A Vulkan-enabled binary still requires a Windows Vulkan loader/driver installation; CPU fallback cannot recover if Windows cannot load a required runtime DLL or a native driver crashes.

GPU-enabled builds route inference automatically between CPU and GPU before each request. The router samples CPU usage, Windows GPU-engine utilization, and Vulkan free memory, then uses measured inference latency to refine decisions. A busy GPU (85% or higher) yields to a CPU with spare capacity; a GPU below 55% with enough memory is preferred. Normal switches require two matching request-time observations and at least 30 seconds on the current backend. Critical GPU memory pressure switches immediately; recoverable GPU failures retry the retained speech on CPU and suppress GPU retries for five minutes. The router conservatively uses the busiest engine across GPUs when Windows counters are available; unavailable counters fall back to memory/capability checks. Switching happens only between requests and releases the old model first, so it does not discard a recording or keep two copies of the same model. CPU thread usage drops under CPU pressure. The microphone remains closed while idle.

VeeType requests below-normal Windows process priority so foreground applications are favored. This is best-effort and does not guarantee zero latency under all workloads.

Application logs are written under `%LOCALAPPDATA%\VeeType` to daily rolling `veetype.log` files.

## One complete edition

VeeType has one fully included edition. Local dictation, hands-free recording, every supported local-model tier, and cloud polishing are available without a VeeType account or license. Cloud providers still require a key from the provider account that you choose, since those services bill independently. Code signing identifies the publisher and helps build reputation, but EV is not universally required and neither EV nor ordinary signing guarantees that SmartScreen warnings disappear.

## Software updates

Use **VeeType Settings → Check for updates** to download and replace the executable from the latest GitHub Release. The updater runs off the settings UI thread. After a successful replacement, close all running VeeType instances and start the app again.

Each release must include a ZIP asset whose name contains the Windows target triple `x86_64-pc-windows-msvc` and whose root contains `VeeType.exe` (for example, `VeeType-x86_64-pc-windows-msvc.zip`). Tag releases with the same version as `Cargo.toml`, such as `v1.0.9`. The installer remains available for new installations.

Every release version is declared once in `Cargo.toml` and must match the VeeType package entry in `Cargo.lock` and `AppVersion` in `setup.iss`. A push to `main` creates the matching Git tag and publishes the installer and OTA ZIP only when that version does not already have a tag. Bump the version before each user-visible improvement: patch for fixes, minor for new capabilities, and major for breaking changes. A manually pushed matching `v*` tag is also supported. The installer contains no models; the first-run Settings wizard downloads a Whisper model. The workflow does not require paid code-signing credentials. Windows may show a SmartScreen warning for unsigned downloads. The executable can use Vulkan-capable NVIDIA, AMD, or Intel GPUs when compatible drivers are installed, and falls back to CPU inference when GPU initialization reports an error. Releases do not require CUDA or provide a separate NVIDIA-only binary.

The Settings wizard downloads Whisper models from the whisper.cpp model repository. Local GGUF text-polishing models must be added separately; without one, dictation still inserts the raw transcript. Review the upstream model licenses before redistributing model files.

## Model assets

Model weights are intentionally excluded from this public repository. Place the Whisper model at `Models\ggml-base.bin` and at least the smallest supported local text model at `Models\qwen2.5-0.5b-instruct-q4_k_m.gguf`. Larger supported model tiers can also be added; VeeType selects a tier based on available memory. Obtain model files from their official sources and comply with their respective licenses.

To compile [`setup.iss`](setup.iss) with Inno Setup, first build the release executable. The installer creates an empty `Models` folder, and the first-run wizard downloads a Whisper model into it.
The generated [`icon.ico`](icon.ico) is used for the tray icon and installer branding.

### Authenticode signing

Signing is optional and requires a code-signing certificate held by your certificate provider, hardware token, or cloud HSM. Never export signing private keys into this repository or CI logs. Install the Windows SDK Signing Tools, then configure an Inno Setup Sign Tool named `MsSign` under **Tools → Configure Sign Tools** with this command:

```text
signtool.exe sign /tr http://timestamp.digicert.com /td SHA256 /fd SHA256 /a $f
```

The `SignTool=MsSign` setting in [`setup.iss`](setup.iss) signs the generated installer. Sign the application executable separately before compiling the installer:

```powershell
signtool.exe sign /tr http://timestamp.digicert.com /td SHA256 /fd SHA256 /a target\release\VeeType.exe
signtool.exe verify /pa /v target\release\VeeType.exe
```

After compiling, verify the installer in the same way with `Output\VeeType_Setup.exe`. A USB token may require its PIN during signing; follow the certificate provider's supported process for cloud-HSM credentials. Keep timestamps enabled so the signature can remain valid after the signing certificate expires.

## Configuration

Copy `config.example.toml` to `config.toml` next to the executable and edit it to suit your setup. The local `config.toml` is ignored by Git so personal prompts and vocabulary are not published.

For cloud polishing, save your Groq or OpenAI key in **VeeType Settings → Cloud API credentials**. Keys are stored in Windows Credential Manager, not in TOML files. As an alternative, `GROQ_API_KEY` or `OPENAI_API_KEY` can be set in the process environment; VeeType uses that only when the corresponding Credential Manager entry is absent.

## GitHub Pages

The landing page is at the repository root as `index.html`. To publish it, open the repository’s **Settings → Pages**, choose **Deploy from a branch**, select the `main` branch and `/ (root)`, then save.
