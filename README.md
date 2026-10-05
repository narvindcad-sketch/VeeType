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

## Build

Requirements: Windows, Rust/Cargo, a working C++ build toolchain for native dependencies, and the model assets listed below.

```powershell
cargo test
cargo build --release
```

The release executable is `target\release\VeeType.exe`.
By default, inference uses CPU execution with up to four worker threads. A CUDA-enabled build can be requested with `cargo build --release --features cuda` on a Windows build machine with the supported CUDA toolkit installed; VeeType falls back to CPU model loading if GPU loading fails.

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
7. Before building the desktop release, set `VEETYPE_SUPABASE_URL`, `VEETYPE_SUPABASE_ANON_KEY`, and `VEETYPE_LICENSE_PUBLIC_KEY_B64` in the build environment. The anon key and public key are not secrets; the signing private key and Lemon Squeezy API/webhook secrets must remain server-side.

The paid checkout variant and Supabase project must be configured before subscription activation can work. Code signing identifies the publisher and helps build reputation, but EV is not universally required and neither EV nor ordinary signing guarantees that SmartScreen warnings disappear.

## Software updates

Use **VeeType Settings → Check for updates** to download and replace the executable from the latest GitHub Release. The updater runs off the settings UI thread. After a successful replacement, close all running VeeType instances and start the app again.

Each release must include a ZIP asset whose name contains the Windows target triple `x86_64-pc-windows-msvc` and whose root contains `VeeType.exe` (for example, `VeeType-x86_64-pc-windows-msvc.zip`). Tag releases with the same version as `Cargo.toml`, such as `v0.1.0`. The installer remains available for new installations.

Pushing a matching `v*` tag runs [the Windows release workflow](.github/workflows/release.yml). It runs the Rust tests, validates the Supabase and Azure signing configuration, downloads pinned Whisper.cpp and Qwen 0.5B model files and their license notices for the installer, signs the CPU executable with Azure Artifact Signing before packaging the installer and OTA ZIP, then builds and signs a CUDA-enabled executable using CUDA Toolkit 12.4.1. The installer is also signed after compilation. The CUDA ZIP includes its model files and CUDA runtime DLLs; extract the complete archive together and use it on a compatible NVIDIA system. The standard installer and OTA ZIP use the signed CPU build and do not require CUDA.

For Azure OIDC signing, configure a Microsoft Entra federated credential for the GitHub repository/ref and grant its service principal the **Artifact Signing Certificate Profile Signer** role. Add `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, and `AZURE_SUBSCRIPTION_ID` as GitHub Actions secrets. Add `AZURE_ARTIFACT_SIGNING_ENDPOINT`, `AZURE_ARTIFACT_SIGNING_ACCOUNT_NAME`, and `AZURE_ARTIFACT_SIGNING_PROFILE_NAME` as Actions variables. The release workflow requires these values and fails before building if they are missing. Follow [Azure Artifact Signing's OIDC setup](https://github.com/Azure/artifact-signing-action/blob/main/docs/OIDC.md); the workflow uses Microsoft's current `azure/artifact-signing-action@v2` and RFC 3161 SHA-256 timestamping.

The model downloads are pinned to specific public Hugging Face revisions. The Whisper model is MIT-licensed and the Qwen model is Apache-2.0; review and comply with the licenses included or linked from their upstream repositories when redistributing them.

## Model assets

Model weights are intentionally excluded from this public repository. Place the Whisper model at `Models\ggml-base.bin` and at least the smallest supported local text model at `Models\qwen2.5-0.5b-instruct-q4_k_m.gguf`. Larger supported model tiers can also be added; VeeType selects a tier based on available memory. Obtain model files from their official sources and comply with their respective licenses.

To compile [`setup.iss`](setup.iss) with Inno Setup, first build the release executable and place the model files in `Models`. The installer includes those local model files; they are not committed to Git.
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
