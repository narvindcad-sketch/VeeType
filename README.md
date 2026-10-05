# VeeType

VeeType is a Windows desktop voice-dictation application. It transcribes speech locally with Whisper, supports vocabulary hints and prompt modes, and can polish text with a local model or an optional Groq/OpenAI-compatible cloud provider.

The [`index.html`](index.html) file is a product landing page. Its interactive demo displays sample text only; it does not record audio or connect to the desktop application.

## Features

- Global hotkey dictation and a native floating status pill.
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

## Model assets

Model weights are intentionally excluded from this public repository. Place the Whisper model at `Models\ggml-base.bin` and at least the smallest supported local text model at `Models\qwen2.5-0.5b-instruct-q4_k_m.gguf`. Larger supported model tiers can also be added; VeeType selects a tier based on available memory. Obtain model files from their official sources and comply with their respective licenses.

To compile [`setup.iss`](setup.iss) with Inno Setup, first build the release executable and place the model files in `Models`. The installer includes those local model files; they are not committed to Git.

## Configuration

Copy `config.example.toml` to `config.toml` next to the executable and edit it to suit your setup. The local `config.toml` is ignored by Git so personal prompts and vocabulary are not published.

For cloud polishing, set `GROQ_API_KEY` or `OPENAI_API_KEY` in the process environment and select the provider in `[settings]` or the optional `[api]` override. Do not put API keys in TOML files.

## GitHub Pages

The landing page is at the repository root as `index.html`. To publish it, open the repository’s **Settings → Pages**, choose **Deploy from a branch**, select the `main` branch and `/ (root)`, then save.
