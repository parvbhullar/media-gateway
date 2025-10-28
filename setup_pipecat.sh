#!/bin/bash

# Setup script for Pipecat audio streaming server

set -e  # Exit on error

echo "=========================================="
echo "Pipecat Server Setup"
echo "=========================================="
echo ""

cd "$(dirname "$0")/pipecat_server" || exit 1

# Check Python version
echo "1. Checking Python..."
if ! command -v python3 &> /dev/null; then
    echo "❌ Python3 not found. Please install Python 3.8 or higher."
    exit 1
fi

PYTHON_VERSION=$(python3 --version 2>&1 | awk '{print $2}')
echo "   ✓ Python $PYTHON_VERSION found"

# Create virtual environment if it doesn't exist
if [ ! -d "venv" ]; then
    echo ""
    echo "2. Creating virtual environment..."
    python3 -m venv venv
    echo "   ✓ Virtual environment created"
else
    echo ""
    echo "2. Virtual environment already exists"
fi

# Activate virtual environment
echo ""
echo "3. Activating virtual environment..."
source venv/bin/activate
echo "   ✓ Virtual environment activated"

# Install core dependencies
echo ""
echo "4. Installing core dependencies..."
echo "   (websockets, pyaudio, numpy, loguru)"

pip install --upgrade pip > /dev/null 2>&1
pip install websockets pyaudio numpy loguru python-dotenv > /dev/null 2>&1

if [ $? -eq 0 ]; then
    echo "   ✓ Core dependencies installed"
else
    echo "   ⚠ Some dependencies may have failed to install"
    echo "   This is often OK - audio streaming may still work"
fi

# Check optional AI dependencies
echo ""
echo "5. Checking optional AI dependencies..."
if pip show pipecat-ai > /dev/null 2>&1; then
    echo "   ✓ pipecat-ai found (AI services available)"
else
    echo "   ⚠ pipecat-ai not installed (AI services disabled)"
    echo "   → This is OK! Audio streaming works without AI"
    echo "   → To enable AI: pip install 'pipecat-ai[silero,deepgram,openai]'"
fi

# Check API keys
echo ""
echo "6. Checking environment configuration..."
if [ -f ".env" ]; then
    echo "   ✓ .env file found"
    source .env
else
    echo "   ℹ No .env file (using environment variables)"
fi

if [ -n "$DEEPGRAM_API_KEY" ] && [ -n "$OPENAI_API_KEY" ]; then
    echo "   ✓ API keys configured (AI services enabled)"
elif [ -n "$DEEPGRAM_API_KEY" ] || [ -n "$OPENAI_API_KEY" ]; then
    echo "   ⚠ Some API keys missing (partial AI support)"
else
    echo "   ℹ No API keys set (audio streaming only mode)"
    echo "   → This is FINE! Audio streaming works without AI"
fi

echo ""
echo "=========================================="
echo "✅ Setup Complete!"
echo "=========================================="
echo ""
echo "To start the server:"
echo "  cd .."
echo "  ./start_pipecat_server.sh"
echo ""
echo "Or manually:"
echo "  cd pipecat_server"
echo "  source venv/bin/activate"
echo "  python pipecat_server.py"
echo ""
echo "Features available:"
echo "  ✓ WebRTC audio streaming"
echo "  ✓ Real-time speaker playback"
echo "  ✓ Binary WebSocket support"
echo "  ✓ Session statistics"
if [ -n "$DEEPGRAM_API_KEY" ] && [ -n "$OPENAI_API_KEY" ]; then
    echo "  ✓ AI services (STT/LLM/TTS)"
else
    echo "  ⚠ AI services (disabled - API keys not set)"
fi
echo ""
