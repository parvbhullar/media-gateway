# Pipecat Media Server

Clean, modular AI-powered media processing server for RustPBX integration.

## Features

- 🎤 **Speech-to-Text**: Deepgram integration for real-time transcription
- 🤖 **Language Model**: OpenAI integration for intelligent responses  
- 🔊 **Text-to-Speech**: Deepgram synthesis for audio generation
- 🌐 **WebSocket API**: Real-time communication with RustPBX
- 📡 **REST API**: Room management and configuration
- 🔧 **Health Monitoring**: Built-in health checks and logging
- ⚡ **Async Architecture**: High-performance concurrent processing

## Quick Start

### 1. Install Dependencies

```bash
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
```

### 2. Configure Environment

```bash
cp .env.example .env
# Edit .env with your API keys
```

### 3. Start Server

```bash
# Option 1: Direct startup
source venv/bin/activate
python start_server.py

# Option 2: Module startup
python -m uvicorn server:app --host 0.0.0.0 --port 8765 --reload
```

### 4. Test Health

```bash
curl http://localhost:8765/health
```

## API Endpoints

### REST API

- `GET /health` - Health check
- `POST /rooms` - Create processing room
- `GET /rooms` - List active rooms
- `GET /rooms/{id}` - Get room details
- `DELETE /rooms/{id}` - Delete room
- `POST /rooms/{id}/prompt` - Update system prompt

### WebSocket Endpoints

- `WS /ws/rustpbx` - RustPBX integration endpoint
- `WS /ws/{client_id}` - General client connections

## Configuration

Environment variables in `.env`:

```bash
# Required API Keys
DEEPGRAM_API_KEY=your_key_here
OPENAI_API_KEY=your_key_here

# Server Settings
PIPECAT_SERVER_HOST=0.0.0.0
PIPECAT_SERVER_PORT=8765
LOG_LEVEL=INFO

# AI Models
LLM_MODEL=gpt-4o-mini
STT_MODEL=nova
TTS_MODEL=aura-asteria-en
```

## RustPBX Integration

The server integrates with RustPBX via WebSocket at `/ws/rustpbx`. Configure RustPBX with:

```toml
[pipecat]
enabled = true
server_url = "ws://localhost:8765/ws/rustpbx"
use_for_ai = true
```

## Development

### Project Structure

```
pipecat_server/
├── __init__.py           # Package initialization
├── main.py              # Original main entry point
├── start_server.py      # Simple startup script
├── config.py            # Configuration management
├── server.py            # FastAPI application
├── models.py            # Data models
├── ai_services.py       # AI service integrations
├── websocket_manager.py # WebSocket connection handling
├── requirements.txt     # Python dependencies
├── .env.example        # Environment template
└── README.md           # This file
```

### Key Components

- **Configuration**: Pydantic-based settings with environment variable support
- **AI Pipeline**: STT → LLM → TTS processing chain
- **WebSocket Manager**: Connection lifecycle and health monitoring
- **REST API**: Room management and server control

## Architecture

```
┌─────────────────┐    WebSocket    ┌──────────────────────┐
│   RustPBX       │ ◄─────────────► │  Pipecat Server      │
│                 │                 │  (FastAPI/WebSocket) │
└─────────────────┘                 └──────────────────────┘
                                            │
                                     ┌──────▼──────┐
                                     │ AI Pipeline │
                                     │             │
                                     │ STT → LLM   │
                                     │   ↓         │
                                     │  TTS        │
                                     └─────────────┘
```

## Monitoring

### Health Check

```bash
curl http://localhost:8765/health
```

### Logs

The server uses structured logging with loguru:
- Server startup and shutdown events
- WebSocket connection lifecycle
- AI processing pipeline status
- Error tracking and debugging

### Connection Stats

```bash
curl http://localhost:8765/rooms
```

## Production Deployment

1. Use environment-specific API keys
2. Configure reverse proxy (nginx)
3. Set up process management (systemd)
4. Enable monitoring and alerting
5. Use secure WebSocket connections (WSS)

## Troubleshooting

### Common Issues

1. **Missing API Keys**: Check `.env` file configuration
2. **Import Errors**: Ensure virtual environment is activated
3. **Port Conflicts**: Change `PIPECAT_SERVER_PORT` if needed
4. **WebSocket Errors**: Check firewall and network settings

### Debug Mode

```bash
LOG_LEVEL=DEBUG python start_server.py
```