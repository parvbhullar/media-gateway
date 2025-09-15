# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

RustPBX is an AI-powered software-defined PBX system built in Rust. It combines traditional telephony (SIP/RTP) with modern AI capabilities (ASR, TTS, LLM) to create intelligent voice communication applications.

## Key Commands

### Building and Running
```bash
# Build the project
cargo build --release

# Run the main PBX server
cargo run --bin rustpbx -- --conf config.toml

# Run tests
cargo test

# Run specific test
cargo test test_name

# Build for production
cargo build --release --features "vad_engines"

# Run linting (requires rustup default stable)
cargo clippy -- -D warnings

# Format code
cargo fmt
```

### Development Utilities
```bash
# Performance testing CLI
cargo run --bin perfcli

# TTS utility
cargo run --bin text2wav -- --text "Hello" --output hello.wav

# ASR utility  
cargo run --bin wav2text -- --input test.wav
```

## Architecture Overview

### Core Modules Structure

1. **SIP Stack** (`src/proxy/`, `src/useragent/`)
   - `proxy/`: Full SIP proxy with auth, registrar, ACL, and call modules
   - `useragent/`: Outbound SIP user agent implementation
   - Uses `rsipstack` for protocol handling

2. **Media Processing** (`src/media/`)
   - Audio codecs: PCMU, PCMA, G.722, Opus, G.729
   - Real-time RTP/RTCP proxying with NAT traversal
   - Jitter buffering and resampling
   - Noise suppression via `nnnoiseless`

3. **AI Integration** (`src/transcription/`, `src/synthesis/`, `src/llm/`)
   - Multi-provider ASR: Deepgram, Tencent Cloud, VoiceAPI
   - Multi-provider TTS: Deepgram, Tencent Cloud, VoiceAPI  
   - OpenAI-compatible LLM proxy
   - Voice Activity Detection (WebRTC VAD, Silero VAD)

4. **Call Management** (`src/call/`)
   - B2BUA (Back-to-Back User Agent) implementation
   - Call state machine and session management
   - WebRTC signaling and media handling

5. **Web Interface** (`src/handler/`, `static/`)
   - Axum-based HTTP server with WebSocket support
   - REST API for call control and management
   - Web UI for testing voice agents

### Configuration System

Configuration is loaded from `config.toml` with environment variable overrides:
- Core settings: HTTP server, SIP ports, media port ranges
- Provider credentials: Deepgram (`DEEPGRAM_API_KEY`), Tencent Cloud (`TENCENT_*`)
- Feature flags in `Cargo.toml` control optional components

### Key Design Patterns

1. **Async Architecture**: Uses Tokio runtime throughout for concurrent I/O
2. **Modular Providers**: ASR/TTS/LLM providers implement common traits for pluggability
3. **Media Pipeline**: Audio flows through codec -> resampling -> processing -> network
4. **Event-Driven Call Control**: State machines handle SIP/WebRTC signaling events

## Development Guidelines

### Adding New Features

1. **New ASR/TTS Provider**: Implement traits in `src/transcription/mod.rs` or `src/synthesis/mod.rs`
2. **New Codec**: Add to `src/media/codec/` and register in `get_codec_manager()`
3. **New API Endpoint**: Add handler in `src/handler/` and route in `create_app()`
4. **New SIP Module**: Add to `src/proxy/modules/` and register in proxy configuration

### Testing Strategy

- Unit tests are colocated with source files (e.g., `src/media/tests/`)
- Integration tests in `src/handler/tests/` test full API flows
- Audio fixtures in `fixtures/` for media processing tests
- Mock SIP/WebRTC endpoints for protocol testing

### Performance Considerations

- Media processing is performance-critical - minimize allocations in audio paths
- Use buffer pools for RTP packet handling
- Async tasks for concurrent call handling
- Feature flags to exclude unused codecs/providers