# Testing RustPBX + Pipecat Integration

## Complete Audio Flow

### Input (Microphone → Pipecat)
1. User speaks into microphone in browser
2. WebRTC captures audio and sends to RustPBX
3. `PipecatProcessor.forward_audio_to_pipecat()` in [pipecat.rs:436](../media-gateway/src/media/track/pipecat.rs#L436):
   - Decodes WebRTC audio (PCMU/PCMA/G.722/Opus)
   - Resamples to 16kHz mono PCM
   - Sends via WebSocket to `ws://localhost:8765/ws/rustpbx`
4. `server_rustpbx.py` receives audio and puts in queue
5. `AudioInputProcessor.process_generator()` yields audio frames
6. Pipecat pipeline processes: STT → LLM → TTS

### Output (Pipecat → Speaker)
1. CartesiaTTS generates audio (TTSAudioRawFrame)
2. `AudioOutputProcessor.process_frame()` in [bot_rustpbx.py:197](bot_rustpbx.py#L197) sends audio via WebSocket
3. `PipecatClient` in RustPBX receives audio response
4. `handle_pipecat_event()` in [pipecat.rs:212](../media-gateway/src/media/track/pipecat.rs#L212):
   - Converts bytes to i16 PCM samples
   - Creates AudioFrame
   - Sends via `packet_sender` to WebRTC track
5. Browser plays audio through speaker

## Test Steps

### 1. Start Pipecat Server
```bash
cd /Users/saurabhtomar/pipecat-voiceagent
./start_rustpbx.sh
```

Expected output:
- "Starting WebSocket server on ws://localhost:8765/ws/rustpbx"
- "Server ready to accept connections"

### 2. Start RustPBX
```bash
cd /Users/saurabhtomar/media-gateway
cargo run --bin rustpbx -- --conf config.toml
```

Expected output:
- RustPBX server starts
- HTTP server on configured port
- WebRTC endpoints ready

### 3. Open Web UI
```
http://localhost:<port>/
```
- Click "Pipecat" tab
- Click "Start Call"
- Allow microphone access

### 4. Test Conversation
1. Say: "Hello, can you hear me?"
2. Watch terminal logs:
   - **Pipecat logs**: Should show "Pipeline StartFrame processed, ready to receive audio"
   - **Pipecat logs**: Should show "Transcription (final): Hello, can you hear me?"
   - **Pipecat logs**: Should show "LLM response: [AI response text]"
   - **Pipecat logs**: Should show "Sending audio response: X bytes"
   - **RustPBX logs**: Should show "Received audio response from Pipecat: X bytes, Y samples"
   - **RustPBX logs**: Should show "Successfully sent audio response to WebRTC track"
3. Hear AI response through speaker

## Verification Checklist

### Audio Input (Mic → Pipecat)
- [ ] RustPBX receives audio from WebRTC
- [ ] Audio forwarded to Pipecat server
- [ ] Pipecat receives binary audio frames
- [ ] No "StartFrame not received yet" errors
- [ ] CartesiaSTT transcribes audio

### AI Processing
- [ ] Transcription appears in logs
- [ ] LLM generates response
- [ ] CartesiaTTS synthesizes speech

### Audio Output (Pipecat → Speaker)
- [ ] TTS audio sent to RustPBX
- [ ] RustPBX converts to AudioFrame
- [ ] AudioFrame sent to WebRTC track
- [ ] User hears AI voice through speaker

## Common Issues

### "StartFrame not received yet"
**Fixed in latest version** - Using generator pattern with `task.queue_frames()`

### "WebSocket handshake failed"
Ensure custom `process_request()` in server_rustpbx.py handles Connection header

### "No audio playback"
Check:
- `packet_sender` is set in PipecatProcessor
- AudioFrame is correctly formatted (16kHz, mono, i16 samples)
- WebRTC track is active

### "Connection refused"
Ensure Pipecat server started before RustPBX

## Environment Variables Required

```bash
export CARTESIA_API_KEY="your_key"
export OPENAI_API_KEY="your_key"
```

See [.env.example](../.env.example) for full list.
