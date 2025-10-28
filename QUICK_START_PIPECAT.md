# Quick Start: Pipecat Audio Streaming

## Current Status
✅ **Build Status**: Compiles successfully
✅ **Pipecat Server**: Running on port 8765
✅ **Configuration**: Properly configured
✅ **Diagnostics**: All checks passed

## How to Use

### 1. Start Pipecat Server (if not running)
```bash
cd pipecat_server
python pipecat_server.py
```

Expected output:
```
🚀 Starting RustPBX-Pipecat server on localhost:8765
✅ Server running on ws://localhost:8765/ws/rustpbx
```

### 2. Start RustPBX
```bash
cargo run --bin rustpbx -- --conf config.toml
```

### 3. Connect a WebRTC Client

Connect your WebRTC client to RustPBX. Once audio starts streaming, you'll see:

**RustPBX logs:**
```
INFO  Starting connection to Pipecat server with reconnection enabled
INFO  ✓ Successfully connected to Pipecat server at ws://localhost:8765/ws/rustpbx
DEBUG Successfully forwarded 200 audio frames to Pipecat server
DEBUG Successfully forwarded 400 audio frames to Pipecat server
```

**Pipecat server logs:**
```
🎤 Starting RustPBX audio session: rustpbx_...
🔊 Audio speaker started for session rustpbx_...
📥 Processing 320 bytes (160 samples)
🔊 Played 320 bytes (frame #1, total: 320 bytes)
📥 Processing 320 bytes (160 samples)
🔊 Played 320 bytes (frame #2, total: 640 bytes)
```

**You should HEAR the WebRTC audio playing on the Pipecat server's speakers!**

## Troubleshooting

### Issue: "WebSocket not connected" errors

This is normal during the first few seconds while connection establishes. The logs will show:
```
DEBUG Skipping frame 0 - waiting for Pipecat connection
DEBUG Skipping frame 100 - waiting for Pipecat connection
```

Then once connected:
```
INFO  ✓ Successfully connected to Pipecat server
DEBUG Successfully forwarded 200 audio frames
```

### Issue: Connection timeout

Check:
1. Pipecat server is running: `./diagnose_pipecat.sh`
2. Port 8765 is not blocked by firewall
3. Config has correct URL: `ws://localhost:8765/ws/rustpbx`

### Issue: No audio on speaker

Check:
1. System volume is not muted
2. PyAudio can access audio device:
   ```bash
   python3 -c "import pyaudio; p = pyaudio.PyAudio(); print(f'Devices: {p.get_device_count()}')"
   ```
3. Pipecat server logs show "🔊 Played X bytes"

### Issue: High CPU usage

This is expected during active audio streaming. The implementation uses:
- Real-time audio processing
- Codec conversion (PCMU/PCMA/G722/Opus → PCM)
- WebSocket transmission
- Speaker playback

## Architecture Flow

```
WebRTC Client
    ↓ (encoded audio: PCMU/PCMA/G722/Opus)
RustPBX
    ↓ (decode to PCM)
PipecatProcessor
    ↓ (WebSocket binary frames)
Pipecat Server
    ↓ (PyAudio)
System Speaker 🔊
```

## Key Features Working

✅ **Multi-codec decoding** - Supports PCMU, PCMA, G.722, Opus
✅ **Binary WebSocket** - Efficient raw audio transmission
✅ **Auto-reconnection** - Exponential backoff (1s → 30s)
✅ **Graceful degradation** - Main pipeline continues if Pipecat fails
✅ **Real-time playback** - Audio plays on Pipecat server speaker
✅ **Session tracking** - Statistics for debugging

## Configuration Options

Edit `config.toml` to customize:

```toml
[pipecat]
enabled = true                    # Enable/disable Pipecat
server_url = "ws://localhost:8765/ws/rustpbx"
use_for_ai = true                 # Use for AI processing
fallback_to_internal = true       # Fallback if unavailable
connection_timeout = 30           # Seconds
debug_logging = false             # Enable verbose logs

[pipecat.reconnect]
enabled = true
max_attempts = 5                  # Max retry attempts
initial_delay = 1                 # Initial delay (seconds)
max_delay = 30                    # Max delay (seconds)
backoff_multiplier = 2.0          # Exponential backoff

[pipecat.audio]
sample_rate = 16000               # 16kHz default
channels = 1                      # Mono
encoding = "linear16"             # PCM format
```

## Logging Levels

To see more/less detail, set `RUST_LOG`:

```bash
# Minimal (errors and warnings only)
RUST_LOG=warn cargo run --bin rustpbx -- --conf config.toml

# Normal (info level - recommended)
RUST_LOG=info cargo run --bin rustpbx -- --conf config.toml

# Verbose (debug level - for troubleshooting)
RUST_LOG=debug cargo run --bin rustpbx -- --conf config.toml

# Very verbose (trace everything)
RUST_LOG=trace cargo run --bin rustpbx -- --conf config.toml
```

## Performance Tips

1. **Lower latency**: Reduce `connection_timeout` to 10-15 seconds
2. **More reliable**: Increase `max_attempts` to 10
3. **Faster recovery**: Reduce `initial_delay` to 0.5 seconds
4. **Debug issues**: Enable `debug_logging = true`

## Next Steps

Once basic audio streaming works:

1. **Integrate AI Pipeline** - Connect STT/LLM/TTS in pipecat_server.py
2. **Add Response Playback** - Send AI-generated audio back to WebRTC client
3. **Enable Metrics** - Track latency, throughput, quality metrics
4. **Production Hardening** - Add TLS, authentication, rate limiting

## Support

- **Full Documentation**: See `PIPECAT_AUDIO_STREAMING.md`
- **Diagnostics**: Run `./diagnose_pipecat.sh`
- **Test Setup**: Run `./test_pipecat_audio.sh`

## Success Criteria

You know it's working when:
1. ✅ RustPBX logs show "✓ Successfully connected to Pipecat server"
2. ✅ Pipecat logs show "🔊 Played X bytes"
3. ✅ You HEAR the WebRTC audio on the Pipecat server's speaker
4. ✅ Frame counter increases: "Successfully forwarded X audio frames"

Enjoy your real-time audio streaming! 🎉
