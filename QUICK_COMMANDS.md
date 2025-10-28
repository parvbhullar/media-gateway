# Quick Command Reference

## 🚀 Start Audio Streaming (First Time)

```bash
# 1. Setup (one-time only)
./setup_pipecat.sh

# 2. Start Pipecat server
./start_pipecat_server.sh

# 3. Start RustPBX (in another terminal)
cargo run --bin rustpbx -- --conf config.toml
```

## 🔄 Daily Use

```bash
# Start Pipecat server
./start_pipecat_server.sh

# Start RustPBX
cargo run --bin rustpbx -- --conf config.toml

# Stop Pipecat server
./stop_pipecat_server.sh
# or press Ctrl+C
```

## 🔧 Troubleshooting Commands

```bash
# Check if server is running
lsof -i :8765

# Stop any server on port 8765
./stop_pipecat_server.sh

# Run diagnostics
./diagnose_pipecat.sh

# Manually stop server
lsof -ti :8765 | xargs kill -9

# Check virtual environment
ls pipecat_server/venv

# Reinstall dependencies
cd pipecat_server
source venv/bin/activate
pip install websockets pyaudio numpy loguru
```

## 📊 Monitoring

```bash
# Watch Pipecat server logs
tail -f /path/to/pipecat/logs

# Watch RustPBX logs with grep
cargo run --bin rustpbx -- --conf config.toml 2>&1 | grep -i pipecat

# Check connections
lsof -i :8765
netstat -an | grep 8765
```

## 🧪 Testing

```bash
# Quick test sequence
./setup_pipecat.sh           # Setup
./diagnose_pipecat.sh        # Diagnose
./start_pipecat_server.sh    # Start server
# In another terminal:
cargo run --bin rustpbx -- --conf config.toml
```

## 🛑 Emergency Stop

```bash
# Kill everything on port 8765
lsof -ti :8765 | xargs kill -9

# Kill all Python processes (careful!)
pkill -9 python

# Kill specific RustPBX process
pkill -9 rustpbx
```

## 📝 Common Issues

### Port 8765 already in use
```bash
./stop_pipecat_server.sh
# or
lsof -ti :8765 | xargs kill -9
```

### Module not found
```bash
cd pipecat_server
source venv/bin/activate
pip install websockets pyaudio numpy loguru
```

### Virtual environment not found
```bash
./setup_pipecat.sh
```

### Can't hear audio
1. Check system volume
2. Verify Pipecat logs show "🔊 Played X bytes"
3. Test PyAudio: `python -c "import pyaudio; print(pyaudio.PyAudio().get_device_count())"`

## 📦 Files Overview

```
./setup_pipecat.sh           - One-time setup
./start_pipecat_server.sh    - Start server
./stop_pipecat_server.sh     - Stop server
./diagnose_pipecat.sh        - Run diagnostics

README_PIPECAT_AUDIO.md      - Complete guide (READ THIS!)
FIXED_AND_READY.md           - What was fixed
QUICK_START_PIPECAT.md       - Quick start guide
```

## 🎯 Verify It's Working

You know it's working when you see:

**Pipecat Server:**
```
🔊 Audio speaker started for session rustpbx_...
🔊 Played 320 bytes (frame #1, total: 320 bytes)
```

**RustPBX:**
```
✓ Successfully connected to Pipecat server
Successfully forwarded 200 audio frames
```

**AND YOU HEAR AUDIO** on the Pipecat server's speaker! 🔊

## 💡 Pro Tips

```bash
# Run server in background
./start_pipecat_server.sh &

# Check if background server is running
jobs
ps aux | grep pipecat_server

# View logs with colors
cargo run --bin rustpbx -- --conf config.toml 2>&1 | grep --color=auto -E 'ERROR|WARN|INFO|$'

# Quick restart
./stop_pipecat_server.sh && ./start_pipecat_server.sh
```

## 🔗 Quick Links

- Full docs: `README_PIPECAT_AUDIO.md`
- Troubleshooting: See "Troubleshooting" section in README
- Configuration: See `config.toml` for RustPBX settings
- Server code: `pipecat_server/pipecat_server.py`

---

**Most Common Workflow:**
```bash
./start_pipecat_server.sh          # Terminal 1
cargo run --bin rustpbx --conf config.toml  # Terminal 2
# Connect WebRTC client → HEAR AUDIO! 🎉
```
