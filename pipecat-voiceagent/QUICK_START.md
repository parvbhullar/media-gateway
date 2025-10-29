# Quick Start Guide - Pipecat Voice Agent with RustPBX

## 🚀 5-Minute Setup

### Step 1: Configure API Keys

```bash
cd /Users/saurabhtomar/pipecat-voiceagent
cp env.example .env
```

Edit `.env`:
```bash
OPENAI_API_KEY=sk-...
CARTESIA_API_KEY=...
```

### Step 2: Install Dependencies

```bash
pip install -r requirements_rustpbx.txt
```

### Step 3: Start Pipecat Server

```bash
./start_rustpbx.sh
```

You should see:
```
✓ Environment variables loaded
✓ Python 3 found
✓ Dependencies installed

Starting Pipecat Voice Agent Server...
Server URL: ws://localhost:8765/ws/rustpbx
```

### Step 4: Start RustPBX (in another terminal)

```bash
cd /Users/saurabhtomar/media-gateway
cargo run --bin rustpbx -- --conf config.toml
```

### Step 5: Open Browser

Navigate to: http://localhost:8080/static/index.html

### Step 6: Enable Pipecat

1. Click **"Pipecat"** tab in Advanced Settings
2. ✅ Check **"Enable Pipecat Media Server"**
3. Verify URL: `ws://localhost:8765/ws/rustpbx`
4. ✅ Check **"Replace internal AI services with Pipecat"**

### Step 7: Start Call

1. Click green **"Call"** button
2. Allow microphone access
3. Say "Hello"
4. Listen for AI response

## 🎯 What Should Happen

### In Pipecat Terminal:
```
New RustPBX connection: rustpbx_1234567890
Starting bot pipeline for connection rustpbx_1234567890
Transcription (partial): Hello
Transcription (final): Hello
LLM response: Hi there! How can I help you today?
TTS started
Sent audio chunk: 32000 bytes
TTS completed
```

### In RustPBX UI Debug Console:
```
10:30:15.123 [SYSTEM] Connected to Pipecat server
10:30:17.456 [ASR] Hello
10:30:18.789 [LLM] Hi there! How can I help you today?
10:30:19.012 [TTS] Processing response
```

### In Your Browser:
- 🎤 You speak: "Hello"
- 💭 AI thinks: [See transcription and LLM response in debug console]
- 🔊 AI speaks: "Hi there! How can I help you today?"

## ⚠️ Common Issues

### Issue: "Connection failed"
**Solution:**
```bash
# Check if Pipecat is running
ps aux | grep server_rustpbx.py

# Restart Pipecat
./start_rustpbx.sh
```

### Issue: "Missing API keys"
**Solution:**
```bash
# Check .env file exists and has keys
cat .env | grep API_KEY
```

### Issue: "No audio response"
**Solution:**
1. Check "Replace internal AI services with Pipecat" is enabled
2. Verify microphone permissions in browser
3. Check browser console for errors (F12)

### Issue: "Module not found"
**Solution:**
```bash
# Reinstall dependencies
pip install --upgrade -r requirements_rustpbx.txt
```

## 📝 Testing Without Full Setup

### Test Pipecat Server Only

```bash
# Install wscat
npm install -g wscat

# Connect to server
wscat -c ws://localhost:8765/ws/rustpbx

# Send test message
{"command": "ping", "timestamp": 1234567890}

# Expected response
{"type": "pong", "timestamp": 1234567890}
```

## 🎛️ Configuration Options

### Change Server Port

Edit in `server_rustpbx.py` or set environment variable:
```bash
SERVER_PORT=9000 python3 server_rustpbx.py
```

### Change AI System Prompt

In RustPBX UI:
1. Go to Pipecat tab
2. Edit "System Prompt" field
3. Start new call

### Enable Debug Logging

```bash
# Set environment variable
export DEBUG=true

# Start server
./start_rustpbx.sh
```

## 📊 Monitoring

### Check Connection Status

**Pipecat Server:**
```bash
# See active connections
ps aux | grep server_rustpbx.py
```

**RustPBX:**
- Look at Debug Console in UI
- Check for "Connected" status next to "Call Status"

### View Logs

**Pipecat:**
- All logs appear in terminal where you ran `start_rustpbx.sh`

**RustPBX:**
- Logs appear in terminal where you ran `cargo run`
- Or check `rustpbx.log` if configured

## 🔄 Restart Everything

```bash
# Terminal 1: Stop Pipecat (Ctrl+C) then
./start_rustpbx.sh

# Terminal 2: Stop RustPBX (Ctrl+C) then
cd /Users/saurabhtomar/media-gateway
cargo run --bin rustpbx -- --conf config.toml

# Browser: Refresh page and try again
```

## ✅ Success Indicators

You know it's working when:
- ✅ Pipecat logs show "New RustPBX connection"
- ✅ RustPBX shows "Connected" status
- ✅ You see transcriptions in debug console
- ✅ You hear AI voice responses
- ✅ No error messages in either terminal

## 🎉 Next Steps

Once everything works:
1. Customize system prompt for your use case
2. Try different conversation topics
3. Monitor performance metrics
4. Experiment with voice settings

## 📚 More Information

- Full documentation: [README_RUSTPBX.md](README_RUSTPBX.md)
- Technical details: [INTEGRATION_SUMMARY.md](INTEGRATION_SUMMARY.md)
- RustPBX docs: https://github.com/restsend/rustpbx

---

**Need help?** Check the troubleshooting section or review the detailed documentation.
