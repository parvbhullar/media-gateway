# Pipecat Integration Guide for RustPBX

This guide explains how to integrate the Pipecat media server with RustPBX for AI-powered voice processing.

## Architecture Overview

```
┌─────────────────┐    WebRTC Audio    ┌──────────────────────┐
│   RustPBX       │  ◄────────────────► │  Pipecat Media       │
│   WebRTC Client │                     │  Server (Python)     │
└─────────────────┘                     └──────────────────────┘
                                               │
                                        ┌──────▼──────┐
                                        │ AI Pipeline │
                                        │             │
                                        │ ┌─────────┐ │
                                        │ │Deepgram │ │ STT
                                        │ │   STT   │ │
                                        │ └─────────┘ │
                                        │      │      │
                                        │ ┌─────▼───┐ │
                                        │ │ OpenAI  │ │ LLM
                                        │ │   LLM   │ │
                                        │ └─────────┘ │
                                        │      │      │
                                        │ ┌─────▼───┐ │
                                        │ │Deepgram │ │ TTS
                                        │ │   TTS   │ │
                                        │ └─────────┘ │
                                        └─────────────┘
┌─────────────────┐    WebRTC Audio    
│   RustPBX       │  ◄────────────────►
│   WebRTC Client │                    
└─────────────────┘                    
                     

## Setup Instructions

### 1. Install Python Pipecat Server

```bash
# Navigate to the pipecat server directory
cd pipecat_server

# Install Python dependencies
pip install -r requirements.txt

# Set up environment variables
cp .env.example .env

# Edit .env with your API keys
nano .env
```

Required environment variables:
```bash
DEEPGRAM_API_KEY=your_deepgram_api_key_here
OPENAI_API_KEY=your_openai_api_key_here
```

### 2. Configure RustPBX

Edit your `config.toml` to enable Pipecat integration:

```toml
# Enable Pipecat integration
[pipecat]
enabled = true
server_url = "ws://localhost:8765/ws/rustpbx"
use_for_ai = true
fallback_to_internal = true
connection_timeout = 30
debug_logging = false

# Pipecat reconnection settings
[pipecat.reconnect]
enabled = true
max_attempts = 5
initial_delay = 1
max_delay = 30
backoff_multiplier = 2.0

# Pipecat audio processing settings
[pipecat.audio]
sample_rate = 16000
channels = 1
frame_size = 160
buffer_size = 10
enable_compression = false
encoding = "linear16"
```

### 3. Start the Services

**Terminal 1 - Start Pipecat Server:**
```bash
cd pipecat_server
python -m pipecat_server.main
```

**Terminal 2 - Start RustPBX:**
```bash
cargo run --bin rustpbx -- --conf config.toml
```

### 4. Test the Integration

1. Open the RustPBX web interface at `http://localhost:8080`
2. Configure the frontend to use Deepgram with your API key
3. Start a WebRTC call
4. Speak into your microphone
5. The audio should be processed by Pipecat and responses should come back

## Configuration Options

### Pipecat Server Configuration

The Pipecat server can be configured via environment variables:

```bash
# Core API Keys
DEEPGRAM_API_KEY=your_key_here
OPENAI_API_KEY=your_key_here

# Server Settings
PIPECAT_SERVER_HOST=0.0.0.0
PIPECAT_SERVER_PORT=8765
LOG_LEVEL=INFO

# AI Model Settings
LLM_MODEL=gpt-4o-mini
LLM_MAX_TOKENS=150
LLM_TEMPERATURE=0.7
TTS_MODEL=aura-asteria-en
STT_MODEL=nova
STT_LANGUAGE=en
```

### RustPBX Integration Settings

In `config.toml`:

- `enabled`: Enable/disable Pipecat integration
- `use_for_ai`: Use Pipecat instead of internal AI processing
- `fallback_to_internal`: Fall back to internal processing if Pipecat fails
- `server_url`: WebSocket URL for Pipecat server
- `debug_logging`: Enable detailed logging for troubleshooting

## API Endpoints

The Pipecat server provides a REST API:

- `GET /health` - Health check
- `POST /rooms` - Create a new processing room
- `GET /rooms` - List active rooms
- `DELETE /rooms/{id}` - Delete a room
- `POST /rooms/{id}/prompt` - Update system prompt
- `WS /ws/rustpbx` - WebSocket endpoint for RustPBX

## Development Mode

For development and testing:

1. **Without Daily.co**: The server works without Daily.co API keys
2. **Local Testing**: Use `ws://localhost:8765/ws/rustpbx` for the server URL
3. **Debug Logging**: Set `debug_logging = true` in RustPBX config
4. **Server Logs**: Check `pipecat_server.log` for detailed logs

## Troubleshooting

### Common Issues

1. **Connection Failed**:
   - Check if Pipecat server is running on port 8765
   - Verify WebSocket URL in RustPBX config
   - Check firewall settings

2. **API Key Errors**:
   - Verify `DEEPGRAM_API_KEY` and `OPENAI_API_KEY` are set correctly
   - Check API key permissions and quotas

3. **Audio Quality Issues**:
   - Ensure sample rates match (16kHz recommended)
   - Check audio encoding settings
   - Verify microphone permissions

4. **High Latency**:
   - Use closest Deepgram region
   - Optimize `LLM_MAX_TOKENS` setting
   - Consider faster models (gpt-3.5-turbo)

### Debug Mode

Enable debug logging:

**RustPBX:**
```toml
[pipecat]
debug_logging = true
```

**Pipecat Server:**
```bash
LOG_LEVEL=DEBUG python -m pipecat_server.main
```

### Health Checks

Check service health:

```bash
# Pipecat server health
curl http://localhost:8765/health

# RustPBX health (if it has a health endpoint)
curl http://localhost:8080/health
```

## Performance Optimization

1. **Use faster models**:
   - LLM: `gpt-3.5-turbo` instead of `gpt-4`
   - STT: `nova` instead of `nova-2-general`

2. **Reduce token limits**:
   ```bash
   LLM_MAX_TOKENS=100  # Instead of 150
   ```

3. **Optimize audio settings**:
   ```toml
   [pipecat.audio]
   frame_size = 160  # 10ms frames for lower latency
   buffer_size = 5   # Smaller buffer
   ```

## Production Deployment

For production deployment:

1. **Use process managers** (systemd, supervisor)
2. **Set up reverse proxy** (nginx)
3. **Configure monitoring** and logging
4. **Use environment-specific API keys**
5. **Set up health checks**

### Example systemd service:

```ini
[Unit]
Description=Pipecat Media Server
After=network.target

[Service]
Type=simple
User=rustpbx
WorkingDirectory=/path/to/media-gateway/pipecat_server
Environment=DEEPGRAM_API_KEY=your_key
Environment=OPENAI_API_KEY=your_key
ExecStart=/usr/bin/python -m pipecat_server.main
Restart=always

[Install]
WantedBy=multi-user.target
```

## Migration from Internal AI

To migrate from internal AI processing to Pipecat:

1. **Gradual Migration**: Set `fallback_to_internal = true` initially
2. **Test Thoroughly**: Verify all voice features work correctly  
3. **Monitor Performance**: Check latency and quality metrics
4. **Disable Internal**: Set `use_for_ai = true` when ready
5. **Remove Fallback**: Set `fallback_to_internal = false` for full migration

## Supported Features

- ✅ Real-time speech-to-text (Deepgram)
- ✅ Language model processing (OpenAI)
- ✅ Text-to-speech synthesis (Deepgram)
- ✅ WebRTC audio streaming
- ✅ Automatic reconnection
- ✅ Fallback to internal processing
- ✅ Metrics and monitoring
- ✅ Multiple simultaneous calls
- ✅ Custom system prompts
- ⏳ Video processing (future)
- ⏳ Multiple AI providers (future)