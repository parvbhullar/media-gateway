# Pipecat Audio Streaming - Complete Guide

## 🎯 What This Does

Streams audio from RustPBX WebRTC clients to a Pipecat server and **plays it on the server's speaker in real-time**.

## ✅ Current Status

**WORKING AND READY TO USE!**

- ✅ Binary WebSocket audio streaming
- ✅ Real-time speaker playback
- ✅ Multi-codec support (PCMU, PCMA, G.722, Opus)
- ✅ Automatic reconnection
- ✅ Session statistics
- ✅ Graceful error handling

## 🚀 Quick Start (3 Steps)

### Step 1: Setup (One-time)

```bash
./setup_pipecat.sh
```

This will:
- Create Python virtual environment
- Install dependencies (websockets, pyaudio, numpy)
- Check for optional AI services

### Step 2: Start Pipecat Server

```bash
./start_pipecat_server.sh
```

You should see:
```
🚀 Starting RustPBX-Pipecat server on localhost:8765
✅ Server running on ws://localhost:8765/ws/rustpbx
🎤 Waiting for RustPBX audio connections...
```

### Step 3: Start RustPBX

In another terminal:
```bash
cargo run --bin rustpbx -- --conf config.toml
```

Then connect your WebRTC client and **speak** - you'll hear the audio on the Pipecat server's speaker! 🔊

## 📋 What You'll See

### Pipecat Server Logs
```
🎤 Starting RustPBX audio session: rustpbx_139876543210
🔊 Audio speaker started for session rustpbx_139876543210
📥 Processing 320 bytes (160 samples) for session rustpbx_139876543210
🔊 Played 320 bytes (frame #1, total: 320 bytes)
📥 Processing 320 bytes (160 samples) for session rustpbx_139876543210
🔊 Played 320 bytes (frame #2, total: 640 bytes)
```

### RustPBX Logs
```
INFO  Starting connection to Pipecat server with reconnection enabled
INFO  ✓ Successfully connected to Pipecat server at ws://localhost:8765/ws/rustpbx
DEBUG Successfully forwarded 200 audio frames to Pipecat server
DEBUG Successfully forwarded 400 audio frames to Pipecat server
```

## 🔧 Troubleshooting

### Issue: "ModuleNotFoundError: No module named 'websockets'"

**Solution:** Activate the virtual environment:
```bash
cd pipecat_server
source venv/bin/activate
python pipecat_server.py
```

Or just use the startup script:
```bash
./start_pipecat_server.sh
```

### Issue: "No module named 'pyaudio'"

**Solution:** Install PyAudio:
```bash
cd pipecat_server
source venv/bin/activate
pip install pyaudio
```

### Issue: PyAudio installation fails on macOS

**Solution:** Install PortAudio first:
```bash
brew install portaudio
cd pipecat_server
source venv/bin/activate
pip install pyaudio
```

### Issue: "Connection refused" or "WebSocket not connected"

**Check 1:** Is the Pipecat server running?
```bash
lsof -i :8765
```

**Check 2:** Is the configuration correct?
```bash
./diagnose_pipecat.sh
```

**Check 3:** Did you start the NEW server (not the old one)?
- ✅ Use: `./start_pipecat_server.sh` or `python pipecat_server.py`
- ❌ Don't use: `python start_server.py` (old server)

### Issue: No audio on speaker

**Check 1:** System volume not muted

**Check 2:** PyAudio can access audio device
```bash
cd pipecat_server
source venv/bin/activate
python -c "import pyaudio; p=pyaudio.PyAudio(); print(f'Audio devices: {p.get_device_count()}')"
```

**Check 3:** Pipecat logs show audio playback
```
🔊 Played 320 bytes (frame #1, total: 320 bytes)
```

If you see this, audio IS being played - check system audio settings.

## 📁 Important Files

- `pipecat_server/pipecat_server.py` - Main server (NEW - use this!)
- `start_pipecat_server.sh` - Easy startup script
- `setup_pipecat.sh` - One-time setup
- `diagnose_pipecat.sh` - Diagnostics tool
- `config.toml` - RustPBX configuration
- `FIXED_AND_READY.md` - What was fixed
- `PIPECAT_AUDIO_STREAMING.md` - Technical documentation

## 🔍 Diagnostics

Run automated checks:
```bash
./diagnose_pipecat.sh
```

This checks:
- ✓ Server is running on port 8765
- ✓ WebSocket endpoint is accessible
- ✓ RustPBX configuration is correct
- ✓ Environment variables (optional)

## ⚙️ Configuration

### RustPBX Configuration (config.toml)

The Pipecat integration is configured and enabled:

```toml
[pipecat]
enabled = true
server_url = "ws://localhost:8765/ws/rustpbx"
use_for_ai = true
fallback_to_internal = true
connection_timeout = 30

[pipecat.reconnect]
enabled = true
max_attempts = 5
initial_delay = 1
max_delay = 30
backoff_multiplier = 2.0

[pipecat.audio]
sample_rate = 16000
channels = 1
encoding = "linear16"
```

### Pipecat Server Configuration

Edit `pipecat_server/pipecat_server.py`:

```python
# Change host/port
server = RustPBXPipecatServer(host="0.0.0.0", port=8765)

# Disable speaker playback
self.enable_speaker_playback = False
```

## 🎓 How It Works

```
┌─────────────────┐
│ WebRTC Client   │  (Speaks into microphone)
└────────┬────────┘
         │ Encoded Audio (PCMU/PCMA/G.722/Opus)
         ↓
┌─────────────────┐
│    RustPBX      │  (Decodes to PCM)
└────────┬────────┘
         │ Binary WebSocket (raw PCM bytes)
         ↓
┌─────────────────┐
│ Pipecat Server  │  (Python)
└────────┬────────┘
         │ PyAudio
         ↓
┌─────────────────┐
│ System Speaker  │  🔊 HEAR AUDIO!
└─────────────────┘
```

## 🚦 Testing Checklist

Before connecting WebRTC client:

- [ ] Pipecat server is running (`lsof -i :8765`)
- [ ] RustPBX is running (`cargo run --bin rustpbx`)
- [ ] Configuration is correct (`./diagnose_pipecat.sh`)
- [ ] System volume is not muted

Expected results:

- [ ] Pipecat shows "🔊 Audio speaker started"
- [ ] Pipecat shows "🔊 Played X bytes"
- [ ] RustPBX shows "✓ Successfully connected"
- [ ] RustPBX shows "Successfully forwarded X audio frames"
- [ ] **YOU HEAR AUDIO** on Pipecat server's speaker

## 📊 Performance

- **Latency:** ~20-50ms
- **Sample Rate:** 16kHz (configurable)
- **Channels:** Mono (1 channel)
- **Frame Size:** 20ms chunks (320 samples @ 16kHz)
- **Buffer:** 10 chunks (200ms)

## 🔐 Security Notes

**Current setup is for development/testing:**
- WebSocket (not WSS)
- No authentication
- Localhost only

**For production:**
- Use WSS (TLS encryption)
- Add authentication
- Bind to specific interfaces
- Add rate limiting

## 📝 Environment Variables (Optional)

Create `pipecat_server/.env`:

```bash
# Optional - only needed for AI services
DEEPGRAM_API_KEY=your_deepgram_key_here
OPENAI_API_KEY=your_openai_key_here
```

**Note:** Audio streaming works WITHOUT these! They're only for AI features (STT/LLM/TTS).

## 🎉 Success!

You know it's working when:

1. Pipecat server logs show: `🔊 Played X bytes`
2. RustPBX logs show: `Successfully forwarded X audio frames`
3. **You HEAR the audio** on the Pipecat server's speaker

Enjoy your real-time audio streaming! 🎊

## 📚 Additional Documentation

- `PIPECAT_AUDIO_STREAMING.md` - Full technical documentation
- `QUICK_START_PIPECAT.md` - Quick reference guide
- `FIXED_AND_READY.md` - What was fixed and why
- `SWITCH_TO_NEW_SERVER.md` - Old vs new server comparison

## 🆘 Getting Help

1. Run diagnostics: `./diagnose_pipecat.sh`
2. Check server logs
3. Check RustPBX logs
4. Verify system audio settings
5. See troubleshooting section above

## 🎯 Next Steps

Once basic audio streaming works:

1. **Test with multiple clients** - Multiple WebRTC connections
2. **Enable AI services** - STT/LLM/TTS integration
3. **Record sessions** - Save audio to files
4. **Add metrics** - Latency, quality monitoring
5. **Production hardening** - TLS, auth, rate limits

Happy streaming! 🚀
