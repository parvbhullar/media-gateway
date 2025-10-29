# Pipecat Voice Agent - RustPBX Integration

This is the modified Pipecat Voice Agent that integrates with RustPBX's WebRTC interface.

## Overview

**Original Setup:**
- Pipecat Voice Agent had its own WebRTC interface (`index.html`)
- Used SmallWebRTC transport for direct WebRTC connections
- Standalone application

**New Setup:**
- RustPBX WebRTC (`static/index.html`) is the **only** entry point
- Pipecat Voice Agent runs as a **media processing service**
- Communicates with RustPBX via WebSocket
- Handles ASR → LLM → TTS pipeline only

## Architecture

```
┌─────────────────┐
│  User Browser   │
│   (Microphone)  │
└────────┬────────┘
         │ WebRTC
         ↓
┌─────────────────────────┐
│  RustPBX WebRTC Server  │
│  (static/index.html)    │
└────────┬────────────────┘
         │ WebSocket (Binary audio + JSON)
         ↓
┌─────────────────────────┐
│  Pipecat Voice Agent    │
│  server_rustpbx.py      │
│                         │
│  ┌─────────────────┐   │
│  │  Audio Input    │   │
│  │       ↓         │   │
│  │  CartesiaSTT    │   │
│  │       ↓         │   │
│  │   OpenAI LLM    │   │
│  │       ↓         │   │
│  │  CartesiaTTS    │   │
│  │       ↓         │   │
│  │  Audio Output   │   │
│  └─────────────────┘   │
└────────┬────────────────┘
         │ WebSocket (Binary audio + JSON)
         ↓
┌─────────────────────────┐
│  RustPBX WebRTC Server  │
└────────┬────────────────┘
         │ WebRTC
         ↓
┌─────────────────┐
│  User Browser   │
│   (Speaker)     │
└─────────────────┘
```

## Installation

1. **Install Python dependencies:**
   ```bash
   cd /Users/saurabhtomar/pipecat-voiceagent
   pip install -r requirements_rustpbx.txt
   ```

2. **Configure environment variables:**
   ```bash
   cp env.example .env
   ```

   Edit `.env` and add your API keys:
   ```
   OPENAI_API_KEY=your_openai_key_here
   CARTESIA_API_KEY=your_cartesia_key_here
   ```

## Usage

### 1. Start the Pipecat Voice Agent Server

```bash
python server_rustpbx.py
```

The server will start on `ws://localhost:8765/ws/rustpbx`

You should see:
```
Starting Pipecat Voice Agent Server for RustPBX
Listening on ws://0.0.0.0:8765/ws/rustpbx
Press Ctrl+C to stop the server
```

### 2. Start RustPBX

```bash
cd /Users/saurabhtomar/media-gateway
cargo run --bin rustpbx -- --conf config.toml
```

### 3. Open RustPBX WebRTC Interface

Open your browser and navigate to:
```
http://localhost:8080/static/index.html
```

### 4. Enable Pipecat in the UI

1. Click on the **"Pipecat"** tab in the Advanced Settings
2. Check **"Enable Pipecat Media Server"**
3. Verify the Server URL is: `ws://localhost:8765/ws/rustpbx`
4. Check **"Replace internal AI services with Pipecat"**
5. Optionally update the System Prompt
6. Click **"Call"** button to start a WebRTC session

## Configuration

### Server Configuration

You can configure the server using environment variables:

```bash
# Server host (default: 0.0.0.0)
SERVER_HOST=0.0.0.0

# Server port (default: 8765)
SERVER_PORT=8765
```

### Audio Configuration

The audio format is fixed to match RustPBX's requirements:
- **Sample Rate:** 16000 Hz
- **Channels:** 1 (mono)
- **Encoding:** 16-bit PCM (linear16)

### AI Services Configuration

Configure in `.env`:

```bash
# OpenAI (for LLM)
OPENAI_API_KEY=your_key

# Cartesia (for STT and TTS)
CARTESIA_API_KEY=your_key
```

## Files

- **`server_rustpbx.py`**: WebSocket server that handles RustPBX connections
- **`bot_rustpbx.py`**: Pipecat pipeline for audio processing (STT → LLM → TTS)
- **`requirements_rustpbx.txt`**: Python dependencies for RustPBX integration
- **`bot.py`** (original): Old version with SmallWebRTC (not used anymore)
- **`index.html`** (original): Old WebRTC interface (not used anymore)

## Message Protocol

### From RustPBX to Pipecat

**Binary Messages:**
- Raw audio data (16-bit PCM, 16kHz, mono)

**JSON Messages:**
```json
{
  "command": "configure",
  "room_id": "rustpbx_123",
  "system_prompt": "You are a helpful assistant..."
}
```

```json
{
  "command": "ping",
  "timestamp": 1234567890
}
```

### From Pipecat to RustPBX

**Transcription (partial/final):**
```json
{
  "type": "transcription",
  "text": "Hello world",
  "is_final": true,
  "timestamp": 1234567890
}
```

**LLM Response:**
```json
{
  "type": "llm_response",
  "text": "How can I help you?",
  "is_complete": false,
  "timestamp": 1234567890
}
```

**Audio Output:**
```json
{
  "type": "audio",
  "audio_data": [1, 2, 3, ...],
  "sample_rate": 16000,
  "channels": 1,
  "frame_id": "audio_1234567890"
}
```

**TTS Events:**
```json
{
  "type": "tts_started",
  "text": "Processing...",
  "timestamp": 1234567890
}
```

**Errors:**
```json
{
  "type": "error",
  "message": "Something went wrong",
  "code": 500,
  "timestamp": 1234567890
}
```

## Troubleshooting

### Connection Issues

1. **Check if Pipecat server is running:**
   ```bash
   ps aux | grep server_rustpbx.py
   ```

2. **Check WebSocket connection:**
   - Open browser console in RustPBX interface
   - Look for WebSocket connection errors

3. **Check RustPBX logs:**
   ```bash
   # Look for Pipecat connection messages
   grep -i pipecat rustpbx.log
   ```

### Audio Issues

1. **No audio processing:**
   - Verify API keys in `.env` file
   - Check Pipecat server logs for errors
   - Ensure RustPBX has "Replace internal AI services with Pipecat" checked

2. **Audio quality problems:**
   - Check network latency between RustPBX and Pipecat server
   - Monitor CPU usage on both servers

### API Key Issues

```bash
# Test OpenAI key
curl https://api.openai.com/v1/models \
  -H "Authorization: Bearer $OPENAI_API_KEY"

# Test Cartesia key
curl https://api.cartesia.ai/voices \
  -H "X-API-Key: $CARTESIA_API_KEY"
```

## Development

### Running in Development Mode

```bash
# Enable debug logging
export DEBUG=true

# Start server
python server_rustpbx.py
```

### Testing Without RustPBX

You can test the WebSocket protocol using `wscat`:

```bash
npm install -g wscat
wscat -c ws://localhost:8765/ws/rustpbx
```

Send binary audio data or JSON commands to test the server.

## Migration from Original Setup

The original `bot.py` and `index.html` are **not used** in this integration. They remain in the repository for reference but are replaced by:

- `bot.py` → `bot_rustpbx.py` (removed SmallWebRTC, added WebSocket audio handling)
- `index.html` → RustPBX's `static/index.html` (RustPBX handles WebRTC)
- Server logic → `server_rustpbx.py` (WebSocket server for RustPBX)

## License

Same as original Pipecat Voice Agent
