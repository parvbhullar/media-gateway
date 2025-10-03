#!/usr/bin/env python3

"""
RustPBX-compatible Pipecat server implementation
Handles raw audio data from RustPBX WebRTC connections
"""

import asyncio
import json
import logging
import os
import sys
import struct
import time
from typing import Dict, Any, Optional

import websockets
import numpy as np

# Pipecat imports for AI processing
from pipecat.audio.vad.silero import SileroVADAnalyzer
from pipecat.processors.aggregators.llm_response import (
    LLMAssistantResponseAggregator,
    LLMUserResponseAggregator,
)
from pipecat.services.deepgram import DeepgramSTTService, DeepgramTTSService
from pipecat.services.openai import OpenAILLMService
from pipecat.vad.vad_analyzer import VADParams

from loguru import logger

# Configure logging
logging.basicConfig(level=logging.DEBUG)
logger.remove()
logger.add(sys.stderr, level="DEBUG")


class RustPBXPipecatServer:
    """Pipecat server specifically designed for RustPBX raw audio integration"""
    
    def __init__(self, host: str = "localhost", port: int = 8765):
        self.host = host
        self.port = port
        self.active_sessions: Dict[str, Dict] = {}
        
    async def create_ai_services(self):
        """Create AI services for processing"""
        
        # Get API keys from environment
        deepgram_api_key = os.getenv("DEEPGRAM_API_KEY")
        openai_api_key = os.getenv("OPENAI_API_KEY")
        
        if not deepgram_api_key:
            raise ValueError("DEEPGRAM_API_KEY environment variable required")
        if not openai_api_key:
            raise ValueError("OPENAI_API_KEY environment variable required")
            
        # Create speech-to-text service (Deepgram)
        stt = DeepgramSTTService(
            api_key=deepgram_api_key,
            model="nova-2",
            language="en-US",
            sample_rate=16000,
            channels=1,
            interim_results=True,
            smart_format=True,
            endpointing=300,  # 300ms silence before finalizing
        )
        
        # Create LLM service (OpenAI)  
        llm = OpenAILLMService(
            api_key=openai_api_key,
            model="gpt-4o-mini",
            max_tokens=150,
            temperature=0.7,
        )
        
        # Create text-to-speech service (Deepgram)
        tts = DeepgramTTSService(
            api_key=deepgram_api_key,
            voice="aura-asteria-en",
            sample_rate=16000,
            encoding="linear16",
        )
        
        # Create VAD (Voice Activity Detection)
        vad = SileroVADAnalyzer(
            params=VADParams(
                stop_secs=0.5,
                start_secs=0.2,
                min_volume=0.6,
            )
        )
        
        return {
            'stt': stt,
            'llm': llm, 
            'tts': tts,
            'vad': vad
        }
        
    async def process_audio_chunk(self, audio_data: bytes, session_id: str) -> Optional[bytes]:
        """Process a single audio chunk through the AI pipeline"""
        
        try:
            services = self.active_sessions[session_id]['services']
            stt = services['stt']
            llm = services['llm']
            tts = services['tts']
            vad = services['vad']
            
            # Convert bytes to numpy array (16-bit PCM)
            audio_samples = np.frombuffer(audio_data, dtype=np.int16)
            
            # Simple audio processing - just log that we received audio
            logger.debug(f"📥 Processing {len(audio_data)} bytes of audio for session {session_id}")
            
            # For now, just acknowledge receipt and send a simple response
            # This shows the connection is working and receiving audio
            
            # Send acknowledgment to RustPBX  
            await self.send_event_to_rustpbx(session_id, {
                "type": "transcription",
                "is_final": False,
                "text": "Processing audio...",
                "timestamp": int(time.time() * 1000)
            })
            
            # TODO: Implement full AI pipeline when basic connection is working
            # The challenge is that Pipecat services expect frame-based processing
            # but we're receiving raw audio chunks from RustPBX
            
            logger.info(f"✅ Audio chunk processed for session {session_id}")
            
        except Exception as e:
            logger.error(f"Error processing audio chunk for session {session_id}: {e}")
            
        return None

    async def send_event_to_rustpbx(self, session_id: str, event: Dict[str, Any]):
        """Send an event back to RustPBX via WebSocket"""
        
        try:
            websocket = self.active_sessions[session_id]['websocket']
            message = json.dumps(event)
            await websocket.send(message)
            logger.debug(f"Sent event to RustPBX: {event['type']}")
            
        except Exception as e:
            logger.error(f"Failed to send event to RustPBX for session {session_id}: {e}")

    async def run_session(self, websocket, path: str):
        """Run a session for raw audio processing from RustPBX"""
        
        session_id = f"rustpbx_{id(websocket)}"
        logger.info(f"🎤 Starting RustPBX audio session: {session_id} for path: {path}")
        
        try:
            # Create AI services for this session
            services = await self.create_ai_services()
            
            # Store session data
            self.active_sessions[session_id] = {
                'websocket': websocket,
                'services': services,
                'started_at': time.time()
            }
            
            # Send ready message to RustPBX
            await self.send_event_to_rustpbx(session_id, {
                "type": "connected",
                "server": "RustPBX-Pipecat",
                "version": "1.0.0",
                "timestamp": int(time.time() * 1000)
            })
            
            logger.info(f"✅ Session {session_id} ready for audio processing")
            
            # Listen for audio data from RustPBX
            async for message in websocket:
                try:
                    if isinstance(message, bytes):
                        # Raw audio data from RustPBX
                        logger.debug(f"Received {len(message)} bytes of audio data from RustPBX")
                        
                        # Process the audio chunk
                        await self.process_audio_chunk(message, session_id)
                        
                    elif isinstance(message, str):
                        # JSON message from RustPBX
                        try:
                            data = json.loads(message)
                            logger.debug(f"Received JSON message from RustPBX: {data}")
                            
                            # Handle different message types if needed
                            if data.get("type") == "ping":
                                await self.send_event_to_rustpbx(session_id, {
                                    "type": "pong", 
                                    "timestamp": data.get("timestamp", int(time.time() * 1000))
                                })
                                
                        except json.JSONDecodeError:
                            logger.warning(f"Invalid JSON message from RustPBX: {message}")
                            
                except Exception as e:
                    logger.error(f"Error processing message in session {session_id}: {e}")
                    break
            
        except Exception as e:
            logger.error(f"Error in session {session_id}: {e}")
            raise
        finally:
            # Clean up session
            if session_id in self.active_sessions:
                del self.active_sessions[session_id]
            logger.info(f"📴 Session {session_id} ended")
            
    async def handle_websocket_connection(self, websocket):
        """Handle incoming WebSocket connections"""
        
        try:
            # Extract path from WebSocket request
            path = websocket.path if hasattr(websocket, 'path') else "/ws/rustpbx"
            
            logger.info(f"🔌 New WebSocket connection from {websocket.remote_address} on path: {path}")
            
            if path == "/ws/rustpbx":
                await self.run_session(websocket, path)
            else:
                logger.warning(f"❌ Unknown path: {path}")
                await websocket.close(code=1008, reason="Unknown path")
                
        except Exception as e:
            logger.error(f"Error handling WebSocket connection: {e}")
            try:
                await websocket.close(code=1011, reason="Server error")
            except:
                pass

    async def start_server(self):
        """Start the RustPBX-compatible Pipecat WebSocket server"""
        
        logger.info(f"🚀 Starting RustPBX-Pipecat server on {self.host}:{self.port}")
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
            
            # Keep server running
            await asyncio.Future()  # Run forever


async def main():
    """Main entry point"""
    
    # Check required environment variables
    required_vars = ["DEEPGRAM_API_KEY", "OPENAI_API_KEY"]
    missing_vars = [var for var in required_vars if not os.getenv(var)]
    
    if missing_vars:
        logger.error(f"Missing required environment variables: {missing_vars}")
        logger.error("Please set them in your .env file or environment")
        sys.exit(1)
    
    # Create and start server
    server = RustPBXPipecatServer(host="localhost", port=8765)
    
    try:
        await server.start_server()
    except KeyboardInterrupt:
        logger.info("Server stopped by user")
    except Exception as e:
        logger.error(f"Server error: {e}")
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())