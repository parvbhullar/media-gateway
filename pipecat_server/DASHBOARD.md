# Pipecat Media Server Dashboard

Interactive web interface for monitoring and testing the Pipecat Media Server with RustPBX integration.

## 🚀 Quick Start

### 1. Start Pipecat Server

```bash
cd pipecat_server
source venv/bin/activate
python start_server.py
```

### 2. Open Dashboard

Open your browser and navigate to:
```
http://localhost:8765
```

## 🎛️ Dashboard Features

### **Control Panel**
- **Audio Test**: Start/stop microphone audio streaming
- **RustPBX Connection**: Test and connect to RustPBX WebSocket
- **AI Pipeline Status**: Real-time status of STT, LLM, and TTS services

### **Real-time Logs**
- Live server logs with color-coded levels
- Auto-scroll and pause functionality
- Clear logs and export capabilities
- Filter by log levels (INFO, ERROR, WARNING, SUCCESS)

### **Statistics**
- Active WebSocket connections
- Messages processed count
- Audio frames processed
- Error count and monitoring

### **Audio Visualization**
- Real-time frequency analysis
- Volume level monitoring
- Waveform display
- Input level meters

## 🔧 Testing WebRTC + AI Pipeline

### Step 1: Start Audio Stream
1. Click **"Start Audio Stream"** button
2. Grant microphone permissions when prompted
3. Monitor audio visualization and volume levels

### Step 2: Connect to RustPBX
1. Ensure RustPBX is running on `localhost:8080`
2. Click **"Test Connection"** to verify HTTP endpoint
3. Click **"Connect to RustPBX"** for WebSocket connection

### Step 3: Test AI Pipeline
1. Watch the AI service status indicators:
   - **STT (Deepgram)**: Speech-to-Text processing
   - **LLM (OpenAI)**: Language model responses
   - **TTS (Deepgram)**: Text-to-Speech synthesis

2. Monitor logs for pipeline activity:
   ```
   [14:30:15] INFO  Audio streaming started
   [14:30:16] SUCCESS  Connected to RustPBX WebSocket
   [14:30:20] INFO  STT: Processing audio frame
   [14:30:21] SUCCESS  LLM: Generated response
   [14:30:22] INFO  TTS: Synthesizing audio response
   ```

## 🌐 WebSocket Endpoints

The dashboard connects to multiple WebSocket endpoints:

- **`/ws/dashboard`**: Real-time dashboard updates and monitoring
- **`/ws/rustpbx`**: RustPBX integration for voice processing
- **`/ws/{client_id}`**: General client connections

## 📊 Monitoring Integration

### Log Messages
Monitor these key log patterns:

```bash
# Successful pipeline processing
✅ STT: Transcribed audio successfully
🤖 LLM: Generated response (150 tokens)
🔊 TTS: Audio synthesis complete

# Connection events
🔌 RustPBX WebSocket connected
📱 Client connected: dashboard
🎵 Audio stream started

# Error monitoring
❌ STT transcription failed: API timeout
⚠️ LLM rate limit exceeded
❌ TTS synthesis error: Invalid audio format
```

### Statistics Tracking
Real-time metrics displayed:

- **Active Connections**: Current WebSocket connections
- **Messages Processed**: Total AI pipeline messages
- **Audio Frames**: Processed audio data count
- **Error Count**: Failed operations tracking

## 🎤 Audio Stream Testing

### WebRTC Flow
1. **Microphone Access**: Browser requests microphone permissions
2. **Audio Analysis**: Real-time frequency and volume analysis
3. **WebRTC Setup**: Peer connection configuration with STUN servers
4. **RustPBX Integration**: Audio streaming to/from RustPBX
5. **AI Processing**: Audio → STT → LLM → TTS → Audio pipeline

### Troubleshooting Audio
- **No Microphone**: Check browser permissions
- **No Audio Visualization**: Verify microphone is active
- **WebRTC Failed**: Check network/firewall settings
- **Poor Quality**: Adjust audio settings in browser

## 🔗 RustPBX Integration Testing

### Connection Flow
```
Dashboard → RustPBX HTTP (Health Check)
Dashboard → RustPBX WebSocket (Signaling)
Dashboard ↔ Pipecat ↔ RustPBX (Audio Pipeline)
```

### Expected Log Sequence
```
[INFO] Testing RustPBX connection
[SUCCESS] RustPBX HTTP endpoint accessible
[INFO] Connecting to RustPBX WebSocket
[SUCCESS] Connected to RustPBX WebSocket
[INFO] WebRTC peer connection configured
[SUCCESS] WebRTC connection established
[INFO] Audio pipeline active
```

## ⌨️ Keyboard Shortcuts

- **Ctrl+L**: Clear logs
- **Ctrl+P**: Pause/resume logs
- **Ctrl+R**: Refresh dashboard
- **F5**: Force refresh

## 🛠️ Development Mode

### Debug Logging
Enable verbose logging by setting:
```bash
LOG_LEVEL=DEBUG python start_server.py
```

### Custom Testing
Use browser console for advanced testing:
```javascript
// Test WebSocket directly
wsManager.send({type: 'test_pipeline', text: 'Hello AI!'});

// Check connection status
console.log(wsManager.ws.readyState);

// Monitor audio levels
console.log(audioVisualizer.getCurrentLevels());
```

## 📱 Mobile Support

The dashboard is responsive and works on mobile devices:
- Touch-friendly controls
- Responsive grid layouts
- Mobile-optimized audio controls
- Swipe gestures for logs

## 🔧 Configuration

Dashboard behavior can be customized via WebSocket messages:

```javascript
// Update visualization settings
wsManager.send({
  type: 'configure',
  config: {
    audio_visualization: true,
    log_level: 'INFO',
    update_interval: 1000
  }
});
```

## 🎯 Production Deployment

For production use:

1. **HTTPS/WSS**: Use secure WebSocket connections
2. **Authentication**: Add user authentication
3. **Rate Limiting**: Implement WebSocket rate limits
4. **Monitoring**: Set up external monitoring
5. **Logging**: Configure structured logging

---

## 🆘 Troubleshooting

### Common Issues

1. **Dashboard Won't Load**
   - Check Pipecat server is running on port 8765
   - Verify static files are in correct directory
   - Check browser console for errors

2. **WebSocket Connection Failed**
   - Ensure server is running and accessible
   - Check firewall settings
   - Verify WebSocket URL is correct

3. **Audio Not Working**
   - Grant microphone permissions
   - Check audio device settings
   - Verify WebRTC support in browser

4. **RustPBX Connection Failed**
   - Ensure RustPBX is running on port 8080
   - Check network connectivity
   - Verify WebSocket endpoint configuration

For more help, check the server logs at the dashboard or server console output.