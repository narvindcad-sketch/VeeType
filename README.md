# 🎙️ VeeType

**VeeType** is a professional-grade, high-performance AI voice dictation and transcription engine built natively for Windows in **Rust**. 

Designed for developers, writers, and executives, VeeType floats invisibly as a glassmorphic UI overlay on your screen. Tap your hotkey, speak naturally, and watch as your raw speech is instantly polished into clean, context-aware prose and typed directly into any application—whether you're writing code in VS Code, drafting a contract in Word, or replying to emails in Outlook.

---

## 🌟 Why Professionals Choose VeeType

* **🔒 100% Private & Local-First:** Powered by `whisper-rs` and local `llama.cpp` (Qwen models) running entirely on your machine's hardware. Your sensitive code and business conversations never leave your device unless you explicitly route them through secure cloud providers.
* **🌐 Universal Language Translation:** Speak fluently in over 100+ languages, with real-time automatic translation and transcription directly into English.
* **🧠 Custom Jargon & Vocabulary Engine:** Stop fighting autocorrect. VeeType conditions the AI audio engine with your custom dictionary so it never misspells product names, technical jargon, or custom syntax.
* **📁 Audio & Video File Transcription:** Right-click the system tray to process `.mp4`, `.mp3`, or `.wav` media files instantly, generating timestamped text transcripts locally.
* **📜 The Dictation Vault:** A secure, local rolling history log equipped with instant full-text search and one-click clipboard recovery so you never lose a brilliant thought.
* **🎛️ Intelligent Context Modes:** Automatically detects your active window to switch formatting rules, or lets you manually toggle between Coding Mode, Professional Business Prose, and Auto-Detection via the System Tray.

---

## 🚀 Technical Architecture
* **Engine:** Written in zero-cost abstraction **Rust** for maximum CPU efficiency and minimal battery drain.
* **Audio Pipeline:** Low-latency `cpal` streaming with advanced Voice Activity Detection (VAD).
* **AI Core:** Hardware-accelerated Whisper transcription paired with local GGUF model execution or Bring-Your-Own-Key (BYOK) cloud options.

---

## 📦 Licensing & Access
VeeType is currently in **Early Access**. 
* Download the installer from our **[Releases](../../releases)** page.
* *Note: Future production builds will transition to a commercial licensing model with advanced enterprise sync features.*
