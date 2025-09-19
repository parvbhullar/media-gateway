#!/bin/bash

# Log monitoring script for RustPBX + Pipecat integration
# This script will monitor both servers' logs in real-time

echo "🔍 RustPBX + Pipecat Log Monitor"
echo "=================================="
echo ""

# Check if both servers are running
echo "📊 Checking server status..."

# Check RustPBX
if pgrep -f "rustpbx" > /dev/null; then
    echo "✅ RustPBX server: RUNNING"
else
    echo "❌ RustPBX server: NOT RUNNING"
fi

# Check Pipecat
if pgrep -f "basic_server.py" > /dev/null; then
    echo "✅ Pipecat server: RUNNING"
else
    echo "❌ Pipecat server: NOT RUNNING"
fi

echo ""
echo "📝 Log Locations:"
echo "  • Pipecat logs: /Users/saurabhtomar/media-gateway/pipecat_server/pipecat.log"
echo "  • RustPBX logs: In terminal output (RUST_LOG=debug)"
echo ""

# Provide monitoring options
echo "🔧 Monitoring Options:"
echo ""
echo "1. Monitor Pipecat logs in real-time:"
echo "   tail -f /Users/saurabhtomar/media-gateway/pipecat_server/pipecat.log"
echo ""
echo "2. Monitor both logs simultaneously (split screen):"
echo "   tmux new-session -d 'tail -f /Users/saurabhtomar/media-gateway/pipecat_server/pipecat.log' \\; split-window -h 'journalctl -f --grep rustpbx' \\; attach"
echo ""
echo "3. Search recent Pipecat logs:"
echo "   grep -i 'websocket\\|error\\|configure' /Users/saurabhtomar/media-gateway/pipecat_server/pipecat.log | tail -10"
echo ""
echo "4. Show connection activity:"
echo "   grep -i 'connected\\|disconnected' /Users/saurabhtomar/media-gateway/pipecat_server/pipecat.log | tail -5"
echo ""

echo "🚀 Starting real-time Pipecat log monitoring..."
echo "   Press Ctrl+C to stop"
echo ""

# Start monitoring Pipecat logs
tail -f /Users/saurabhtomar/media-gateway/pipecat_server/pipecat.log