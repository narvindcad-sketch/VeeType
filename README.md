# VeeType

VeeType is a Windows desktop voice-dictation application. It transcribes speech locally with Whisper, supports vocabulary hints and prompt modes, and can polish text with a local model or an optional Groq/OpenAI-compatible cloud provider.

The [`index.html`](index.html) file is a product landing page. Its interactive demo displays sample text only; it does not record audio or connect to the desktop application.

## Features

- Event-driven Win32 `WM_HOTKEY` dictation with no idle key polling, and a native, click-through floating pill with an animated indigo/violet waveform and processing indicator.
- Single-instance tray behavior; launching VeeType again opens Settings in the running instance.
- Text insertion does not use the system clipboard, so existing clipboard formats and contents are left untouched.
- Microphone capture uses WebRTC VAD to filter non-speech frames, with a 300 ms pre-roll and 450 ms speech hangover to help preserve word edges.
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

VeeType requests below-normal Windows process priority so foreground applications are favored. This is best-effort and does not guarantee zero latency under all workloads.

Application logs are written under `%LOCALAPPDATA%\VeeType` to daily rolling `veetype.log` files.

## Pro subscriptions and licensing

Local basic dictation remains free. A signed Pro license unlocks cloud providers, hands-free dictation, and local models larger than 0.5B. Licenses are Ed25519-signed, cached in Windows Credential Manager, verified locally, and expire no later than seven days after issuance (or at the end of the paid period). Desktop clients contain only the public key. Local checks reduce casual sharing but cannot make a locally controlled executable immune to patching or clock manipulation.

The Supabase project uses email/password Auth, Edge Functions, and the SQL migration under `supabase/migrations`. Lemon Squeezy checkout is created by an authenticated Edge Function and carries the Supabase user ID in checkout custom data; the webhook verifies Lemon Squeezy's HMAC signature before changing subscription state. The private `veetype_subscriptions` table is the source of truth used by license issuance; a separate `profiles.subscription_status` column is not required.

Deployment outline:

1. Install the Supabase CLI and Deno. Generate a one-time Ed25519 pair with `deno run supabase/scripts/generate-license-keys.ts`. Store the private output only as `VEETYPE_LICENSE_PRIVATE_KEY_PKCS8_B64` in Supabase secrets; pass the public output into the desktop build environment. For GitHub Actions releases, add `VEETYPE_SUPABASE_URL`, `VEETYPE_SUPABASE_ANON_KEY`, and `VEETYPE_LICENSE_PUBLIC_KEY_B64` as repository Actions variables. Never commit the private key.
2. Link the project and apply the schema: `supabase link --project-ref <project-ref>` then `supabase db push`.
3. Enable email/password sign-in in Supabase Auth. Configure the Lemon Squeezy Pro product and use the matching store and variant IDs for the environment you are deploying (test or live).
4. Set `LEMON_SQUEEZY_API_KEY`, `LEMON_SQUEEZY_STORE_ID`, `LEMON_SQUEEZY_PRO_VARIANT_ID`, `LEMON_SQUEEZY_WEBHOOK_SECRET`, and `VEETYPE_LICENSE_PRIVATE_KEY_PKCS8_B64` with `supabase secrets set`. The webhook secret must exactly match the signing secret configured for the Lemon Squeezy webhook. Supabase supplies its URL and service keys to deployed functions.
5. Deploy `issue-license`, `create-checkout`, and `lemon-squeezy-webhook` with `supabase functions deploy <function-name>`. Configure the Lemon Squeezy webhook URL as `https://<project-ref>.supabase.co/functions/v1/lemon-squeezy-webhook` and subscribe to `subscription_created`, `subscription_updated`, `subscription_cancelled`, `subscription_expired`, `subscription_paused`, `subscription_resumed`, `subscription_unpaused`, `subscription_payment_success`, `subscription_payment_failed`, and `subscription_payment_recovered`.
6. Purchases must start from the authenticated VeeType checkout flow so the checkout includes the buyer's Supabase user ID. After Lemon Squeezy delivers the signed webhook and the subscription is recorded, the user can refresh their license in Settings. The desktop client never receives the webhook secret or signing private key.
7. For account sign-in and Pro license verification, set `VEETYPE_SUPABASE_URL`, `VEETYPE_SUPABASE_ANON_KEY`, and `VEETYPE_LICENSE_PUBLIC_KEY_B64` in the build environment. They are optional for a basic local-dictation build. The release workflow warns and disables Pro sign-in/license verification when they are missing or invalid, rather than failing the build. The anon key and public key are not secrets; the signing private key and Lemon Squeezy API/webhook secrets must remain server-side.

The paid checkout variant and Supabase project must be configured before subscription activation can work. Code signing identifies the publisher and helps build reputation, but EV is not universally required and neither EV nor ordinary signing guarantees that SmartScreen warnings disappear.

## Software updates

Use **VeeType Settings → Check for updates** to download and replace the executable from the latest GitHub Release. The updater runs off the settings UI thread. After a successful replacement, close all running VeeType instances and start the app again.

Each release must include a ZIP asset whose name contains the Windows target triple `x86_64-pc-windows-msvc` and whose root contains `VeeType.exe` (for example, `VeeType-x86_64-pc-windows-msvc.zip`). Tag releases with the same version as `Cargo.toml`, such as `v1.0.9`. The installer remains available for new installations.

Every ordinary push to `main` runs [the Windows release workflow](.github/workflows/release.yml). It increments the patch version, synchronizes the application, lockfile, and installer metadata, creates a matching Git tag, then runs the Rust tests and publishes the installer and OTA ZIP. A manually pushed matching `v*` tag is also supported. The installer contains no models; the first-run Settings wizard downloads a Whisper model. Missing or invalid Supabase licensing configuration disables account sign-in and Pro license verification for that build, but does not stop a basic local-dictation release. The workflow does not require paid code-signing credentials. Windows may show a SmartScreen warning for unsigned downloads. The executable can use Vulkan-capable NVIDIA, AMD, or Intel GPUs when compatible drivers are installed, and falls back to CPU inference when GPU initialization reports an error. Releases do not require CUDA or provide a separate NVIDIA-only binary.

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
