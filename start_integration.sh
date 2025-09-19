#!/bin/bash

# RustPBX + Pipecat Integration Startup Script
# This script starts both RustPBX and Pipecat servers

set -e  # Exit on any error

echo "🚀 Starting RustPBX + Pipecat Media Server Integration"
echo "========================================================"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if we're in the right directory
if [ ! -f "config.toml" ]; then
    print_error "config.toml not found. Please run this script from the media-gateway directory."
    exit 1
fi

# Check if pipecat_server directory exists
if [ ! -d "pipecat_server" ]; then
    print_error "pipecat_server directory not found."
    exit 1
fi

print_status "Checking prerequisites..."

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    print_error "Cargo not found. Please install Rust."
    exit 1
fi

# Check if Python virtual environment exists
if [ ! -d "pipecat_server/venv" ]; then
    print_warning "Python virtual environment not found. Creating it..."
    cd pipecat_server
    python3 -m venv venv
    source venv/bin/activate
    pip install -r requirements.txt
    cd ..
    print_status "Virtual environment created and dependencies installed."
fi

# Check if .env file exists
if [ ! -f "pipecat_server/.env" ]; then
    print_warning ".env file not found. Creating from template..."
    cp pipecat_server/.env.example pipecat_server/.env
    print_error "Please edit pipecat_server/.env with your API keys before continuing."
    print_error "Required: DEEPGRAM_API_KEY and OPENAI_API_KEY"
    exit 1
fi

# Function to cleanup background processes
cleanup() {
    print_status "Shutting down servers..."
    if [ ! -z "$PIPECAT_PID" ]; then
        kill $PIPECAT_PID 2>/dev/null || true
        print_status "Pipecat server stopped"
    fi
    if [ ! -z "$RUSTPBX_PID" ]; then
        kill $RUSTPBX_PID 2>/dev/null || true
        print_status "RustPBX server stopped"
    fi
    exit 0
}

# Set up signal handlers
trap cleanup SIGINT SIGTERM

print_status "Starting Pipecat Media Server..."

# Start Pipecat server in background
cd pipecat_server
source venv/bin/activate
python start_server.py &
PIPECAT_PID=$!
cd ..

# Wait a bit for Pipecat to start
sleep 3

# Check if Pipecat server is running
if ! curl -s http://localhost:8765/health > /dev/null; then
    print_error "Pipecat server failed to start or is not responding"
    cleanup
fi

print_status "✅ Pipecat server started successfully on http://localhost:8765"

print_status "Starting RustPBX server..."

# Build and start RustPBX server in background
cargo run --bin rustpbx -- --conf config.toml &
RUSTPBX_PID=$!

# Wait a bit for RustPBX to start
sleep 5

# Check if RustPBX server is running (check if port is open)
if ! nc -z localhost 8080 2>/dev/null; then
    print_error "RustPBX server failed to start or is not responding on port 8080"
    cleanup
fi

print_status "✅ RustPBX server started successfully on http://localhost:8080"

echo ""
echo "🎉 Integration Setup Complete!"
echo "==============================="
echo ""
echo "📡 Services Running:"
echo "   • Pipecat Media Server: http://localhost:8765"
echo "   • RustPBX WebRTC Interface: http://localhost:8080"
echo ""
echo "🎛️ Next Steps:"
echo "   1. Open web interface: http://localhost:8080"
echo "   2. Go to 'Pipecat' tab and enable integration"
echo "   3. Click 'Call' button to start voice conversation"
echo "   4. Monitor logs in this terminal"
echo ""
echo "🔍 Monitoring:"
echo "   • Pipecat Dashboard: http://localhost:8765"
echo "   • Integration Test: python test_integration.py"
echo ""
echo "⚡ Performance Tips:"
echo "   • Use Chrome/Edge for best WebRTC performance"
echo "   • Ensure stable internet connection"
echo "   • Grant microphone permissions when prompted"
echo ""
echo "🛑 To stop both servers: Press Ctrl+C"
echo ""

# Wait for user to stop the servers
print_status "Servers are running. Press Ctrl+C to stop..."

# Keep the script running and show live status
while true; do
    sleep 30
    
    # Check if both servers are still running
    if ! kill -0 $PIPECAT_PID 2>/dev/null; then
        print_error "Pipecat server stopped unexpectedly"
        cleanup
    fi
    
    if ! kill -0 $RUSTPBX_PID 2>/dev/null; then
        print_error "RustPBX server stopped unexpectedly"
        cleanup
    fi
    
    print_status "Status: Both servers running normally"
done