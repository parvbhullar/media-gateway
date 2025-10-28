#!/bin/bash

# Quick start script for Pipecat audio streaming server

cd "$(dirname "$0")/pipecat_server" || exit 1

echo "🚀 Starting Pipecat Audio Streaming Server..."
echo ""

# Check if port 8765 is already in use
if lsof -i :8765 > /dev/null 2>&1; then
    echo "⚠️  Port 8765 is already in use"
    echo ""
    echo "Options:"
    echo "  1. Kill the old server:  kill -9 \$(lsof -ti :8765)"
    echo "  2. Or run this command:  lsof -ti :8765 | xargs kill -9"
    echo ""
    read -p "Kill old server and continue? (y/N): " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        lsof -ti :8765 | xargs kill -9 2>/dev/null
        echo "✓ Stopped old server"
        sleep 1
    else
        echo "Exiting..."
        exit 1
    fi
fi

echo "This server will:"
echo "  ✓ Receive audio from RustPBX WebRTC clients"
echo "  ✓ Play audio on system speaker"
echo "  ✓ Track session statistics"
echo ""

# Check if virtual environment exists
if [ -d "venv" ]; then
    echo "✓ Activating virtual environment..."
    source venv/bin/activate
else
    echo "⚠️  Virtual environment not found!"
    echo "   Run ./setup_pipecat.sh first"
    exit 1
fi

echo ""
echo "Press Ctrl+C to stop the server"
echo "========================================"
echo ""

python pipecat_server.py
