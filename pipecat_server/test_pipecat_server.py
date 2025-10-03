#!/usr/bin/env python3

"""
Simple test server for RustPBX audio streaming verification
Tests basic WebSocket connection and audio data reception
"""

import asyncio
import json
import logging
import sys
import time

import websockets
from loguru import logger

# Configure logging
logging.basicConfig(level=logging.DEBUG)
logger.remove()
logger.add(sys.stderr, level="DEBUG")


class TestPipecatServer:
    """Simple test server to verify RustPBX audio streaming"""
    
    def __init__(self, host: str = "localhost", port: int = 8765):
        self.host = host
        self.port = port
        self.active_sessions = {}
        
    async def send_event_to_rustpbx(self, session_id: str, event: dict):
        """Send an event back to RustPBX via WebSocket"""
        
        try:
            websocket = self.active_sessions[session_id]['websocket']
            message = json.dumps(event)
            await websocket.send(message)
            logger.debug(f"📤 Sent event to RustPBX: {event['type']}")
            
        except Exception as e:
            logger.error(f"❌ Failed to send event to RustPBX for session {session_id}: {e}")

    async def process_audio_chunk(self, audio_data: bytes, session_id: str):
        """Process a single audio chunk - just log and acknowledge"""
        
        logger.info(f"✅ SUCCESS! Received {len(audio_data)} bytes of audio data from session {session_id}")
        
        # Analyze the audio data
        if len(audio_data) > 0:
            # Sample first few bytes to see the data
            sample_str = ' '.join([f"{b:02x}" for b in audio_data[:min(16, len(audio_data))]])
            logger.info(f"   📊 Audio sample (first {min(16, len(audio_data))} bytes): {sample_str}")
            
            # Check if it looks like valid audio (non-zero values)
            non_zero_count = sum(1 for b in audio_data if b != 0)
            logger.info(f"   🔊 Non-zero bytes: {non_zero_count}/{len(audio_data)} ({non_zero_count/len(audio_data)*100:.1f}%)")
            
        # Send acknowledgment to RustPBX  
        await self.send_event_to_rustpbx(session_id, {
            "type": "transcription",
            "is_final": False,
            "text": f"✅ Audio streaming working! Received {len(audio_data)} bytes",
            "timestamp": int(time.time() * 1000)
        })
        
        # Update stats
        self.active_sessions[session_id]['audio_chunks_received'] += 1
        self.active_sessions[session_id]['total_audio_bytes'] += len(audio_data)
        
        # Log success every 10 chunks
        chunks = self.active_sessions[session_id]['audio_chunks_received']
        if chunks % 10 == 0:
            total_mb = self.active_sessions[session_id]['total_audio_bytes'] / (1024 * 1024)
            logger.info(f"🎉 Milestone: {chunks} audio chunks received, {total_mb:.2f}MB total")

    async def run_session(self, websocket, path: str):
        """Run a session for raw audio processing from RustPBX"""
        
        session_id = f"test_{int(time.time())}"
        client_address = websocket.remote_address if hasattr(websocket, 'remote_address') else 'unknown'
        
        logger.info(f"🔌 New session {session_id} from {client_address} on path: {path}")
        
        try:
            # Store session data
            self.active_sessions[session_id] = {
                'websocket': websocket,
                'started_at': time.time(),
                'audio_chunks_received': 0,
                'total_audio_bytes': 0,
                'json_messages_received': 0
            }
            
            # Send ready message to RustPBX
            await self.send_event_to_rustpbx(session_id, {
                "type": "connected",
                "server": "Test-Pipecat-Server",
                "version": "1.0.0",
                "session_id": session_id,
                "timestamp": int(time.time() * 1000)
            })
            
            logger.info(f"✅ Session {session_id} ready for audio processing")
            
            # Listen for messages from RustPBX
            async for message in websocket:
                try:
                    if isinstance(message, bytes):
                        # Raw audio data from RustPBX
                        logger.debug(f"📥 Received {len(message)} bytes of raw audio data")
                        await self.process_audio_chunk(message, session_id)
                        
                    elif isinstance(message, str):
                        # JSON message from RustPBX
                        try:
                            data = json.loads(message)
                            message_type = data.get("type", "unknown")
                            
                            if message_type == "Audio":
                                # This is the audio message from RustPBX!
                                audio_frame = data.get("Audio", {})
                                audio_data = audio_frame.get("audio_data", [])
                                sample_rate = audio_frame.get("sample_rate", 16000)
                                channels = audio_frame.get("channels", 1)
                                frame_id = audio_frame.get("frame_id", "unknown")
                                
                                logger.info(f"🎵 Received AUDIO FRAME: {len(audio_data)} samples, {sample_rate}Hz, {channels}ch, ID: {frame_id}")
                                
                                # Convert audio data to bytes and process
                                audio_bytes = bytes(audio_data)  # Convert from array to bytes
                                await self.process_audio_chunk(audio_bytes, session_id)
                                
                            else:
                                logger.info(f"📨 Received JSON message: {message_type}")
                                logger.debug(f"   Full message: {data}")
                                
                                self.active_sessions[session_id]['json_messages_received'] += 1
                                
                                # Handle different message types
                                if data.get("type") == "ping":
                                    await self.send_event_to_rustpbx(session_id, {
                                        "type": "pong", 
                                        "timestamp": data.get("timestamp", int(time.time() * 1000))
                                    })
                                elif data.get("type") == "status_request":
                                    session_stats = self.active_sessions[session_id]
                                    await self.send_event_to_rustpbx(session_id, {
                                        "type": "status_response",
                                        "session_id": session_id,
                                        "audio_chunks_received": session_stats['audio_chunks_received'],
                                        "total_audio_bytes": session_stats['total_audio_bytes'],
                                        "json_messages_received": session_stats['json_messages_received'],
                                        "uptime_seconds": int(time.time() - session_stats['started_at']),
                                        "timestamp": int(time.time() * 1000)
                                    })
                                    
                        except json.JSONDecodeError:
                            logger.warning(f"⚠️  Invalid JSON message from RustPBX: {message}")
                            
                except Exception as e:
                    logger.error(f"❌ Error processing message in session {session_id}: {e}")
                    break
            
        except Exception as e:
            logger.error(f"❌ Error in session {session_id}: {e}")
        finally:
            # Clean up session and log stats
            if session_id in self.active_sessions:
                stats = self.active_sessions[session_id]
                duration = int(time.time() - stats['started_at'])
                logger.info(f"📊 Session {session_id} stats:")
                logger.info(f"   Duration: {duration}s")
                logger.info(f"   Audio chunks: {stats['audio_chunks_received']}")
                logger.info(f"   Total audio bytes: {stats['total_audio_bytes']}")
                logger.info(f"   JSON messages: {stats['json_messages_received']}")
                
                del self.active_sessions[session_id]
            logger.info(f"📴 Session {session_id} ended")

    async def handle_websocket_connection(self, websocket):
        """Handle incoming WebSocket connections"""
        
        try:
            # Extract path from WebSocket request
            path = getattr(websocket, 'path', '/ws/rustpbx')
            
            logger.info(f"🔌 New WebSocket connection on path: {path}")
            
            if path == "/ws/rustpbx":
                await self.run_session(websocket, path)
            else:
                logger.warning(f"❌ Unknown path: {path}")
                await websocket.close(code=1008, reason="Unknown path")
                
        except Exception as e:
            logger.error(f"❌ Error handling WebSocket connection: {e}")
            try:
                await websocket.close(code=1011, reason="Server error")
            except:
                pass

    async def start_server(self):
        """Start the test WebSocket server"""
        
        logger.info(f"🚀 Starting Test Pipecat Server on {self.host}:{self.port}")
        logger.info("📡 Ready to receive raw audio from RustPBX WebRTC connections")
        
        # Start WebSocket server
        async with websockets.serve(
            self.handle_websocket_connection, 
            self.host, 
            self.port,
            ping_interval=30,
            ping_timeout=10,
            close_timeout=10,
        ):
            logger.info(f"✅ Server running on ws://{self.host}:{self.port}/ws/rustpbx")
            logger.info("🎤 Waiting for RustPBX audio connections...")
            logger.info("💡 This is a test server - it will just log received audio data")
            
            # Keep server running
            await asyncio.Future()  # Run forever


async def main():
    """Main entry point"""
    
    logger.info("🧪 Test Pipecat Server - RustPBX Audio Streaming Verification")
    
    # Create and start server
    server = TestPipecatServer(host="localhost", port=8765)
    
    try:
        await server.start_server()
    except KeyboardInterrupt:
        logger.info("🛑 Server stopped by user")
    except Exception as e:
        logger.error(f"❌ Server error: {e}")
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())