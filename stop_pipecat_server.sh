#!/bin/bash

# Stop Pipecat server running on port 8765

echo "Stopping Pipecat server..."

if lsof -i :8765 > /dev/null 2>&1; then
    PID=$(lsof -ti :8765)
    echo "Found server on port 8765 (PID: $PID)"
    kill -9 $PID 2>/dev/null
    sleep 1

    if lsof -i :8765 > /dev/null 2>&1; then
        echo "❌ Failed to stop server"
        exit 1
    else
        echo "✅ Server stopped successfully"
    fi
else
    echo "ℹ️  No server running on port 8765"
fi
