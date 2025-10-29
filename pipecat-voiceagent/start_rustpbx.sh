#!/bin/bash

# Start script for Pipecat Voice Agent (RustPBX Integration)

echo "============================================"
echo "Pipecat Voice Agent for RustPBX"
echo "============================================"
echo ""

# Check if .env file exists
if [ ! -f .env ]; then
    echo "❌ Error: .env file not found!"
    echo ""
    echo "Please create a .env file with your API keys:"
    echo "  cp env.example .env"
    echo ""
    echo "Then edit .env and add:"
    echo "  OPENAI_API_KEY=your_openai_key"
    echo "  CARTESIA_API_KEY=your_cartesia_key"
    echo ""
    exit 1
fi

# Check for required API keys
source .env

if [ -z "$OPENAI_API_KEY" ]; then
    echo "❌ Error: OPENAI_API_KEY not set in .env file"
    exit 1
fi

if [ -z "$CARTESIA_API_KEY" ]; then
    echo "❌ Error: CARTESIA_API_KEY not set in .env file"
    exit 1
fi

echo "✓ Environment variables loaded"
echo ""

# Check if Python 3 is installed
if ! command -v python3 &> /dev/null; then
    echo "❌ Error: Python 3 is not installed"
    exit 1
fi

echo "✓ Python 3 found: $(python3 --version)"
echo ""

# Check if required packages are installed
echo "Checking Python dependencies..."
if ! python3 -c "import websockets" 2>/dev/null; then
    echo "❌ Missing dependencies. Installing..."
    pip install -r requirements_rustpbx.txt
    if [ $? -ne 0 ]; then
        echo "❌ Failed to install dependencies"
        exit 1
    fi
    echo "✓ Dependencies installed"
else
    echo "✓ Dependencies already installed"
fi

echo ""
echo "Starting Pipecat Voice Agent Server..."
echo "Server URL: ws://localhost:8765/ws/rustpbx"
echo ""
echo "Next steps:"
echo "  1. Start RustPBX server in another terminal"
echo "  2. Open http://localhost:8080/static/index.html"
echo "  3. Enable Pipecat in the UI settings"
echo "  4. Start a call"
echo ""
echo "Press Ctrl+C to stop the server"
echo "============================================"
echo ""

# Start the server
python3 server_rustpbx.py
