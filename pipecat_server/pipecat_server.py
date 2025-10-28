#!/usr/bin/env python3

"""
RustPBX-compatible Pipecat server implementation
Handles raw audio data from RustPBX WebRTC connections and plays on speaker
"""

import asyncio
import json
import logging
import os
import sys
import struct
import time
from typing import Dict, Any, Optional
from collections import deque

import websockets
import numpy as np
import pyaudio
from scipy import signal

from loguru import logger

# Pipecat AI imports are optional - they will be loaded on-demand if available
PIPECAT_AI_AVAILABLE = False

# Configure logging
logging.basicConfig(level=logging.DEBUG)
logger.remove()
logger.add(sys.stderr, level="DEBUG")

# Audio configuration constants
INPUT_SAMPLE_RATE = 16000  # Incoming audio from RustPBX
OUTPUT_SAMPLE_RATE = 44100  # Native macOS audio rate
CHANNELS = 1  # Incoming audio is mono
OUTPUT_CHANNELS = 2  # Most speakers are stereo
SAMPLE_WIDTH = 2  # 16-bit = 2 bytes
CHUNK_SIZE = 320  # 20ms at 16kHz
BUFFER_SIZE = 10  # Number of chunks to buffer

# Resampling ratio
RESAMPLE_RATIO = OUTPUT_SAMPLE_RATE / INPUT_SAMPLE_RATE  # 2.75625


class AudioSpeakerPlayer:
    """Handles audio playback to system speaker with buffering"""

    def __init__(self, session_id: str):
        self.session_id = session_id
        self.audio = pyaudio.PyAudio()
        self.stream = None
        self.buffer = asyncio.Queue(maxsize=20)  # Buffer up to 20 frames (400ms)
        self.is_playing = False
        self.total_bytes_played = 0
        self.total_frames_played = 0
        self.playback_task = None
        self.underrun_count = 0
        self.overrun_count = 0

    async def start(self):
        """Start audio output stream"""
        try:
            # Get default output device info
            default_output = self.audio.get_default_output_device_info()
            output_channels = int(default_output['maxOutputChannels'])

            logger.info(f"🔊 Opening audio device: {default_output['name']}")
            logger.info(f"   Output channels: {output_channels}, Sample rate: {OUTPUT_SAMPLE_RATE}Hz (resampling from {INPUT_SAMPLE_RATE}Hz)")

            # Open stream with optimal settings for smooth playback
            self.stream = self.audio.open(
                format=pyaudio.paInt16,
                channels=output_channels,  # Use device's native channels (usually 2 for stereo)
                rate=OUTPUT_SAMPLE_RATE,  # Use native system sample rate!
                output=True,
                frames_per_buffer=int(CHUNK_SIZE * RESAMPLE_RATIO) * 2,  # Larger buffer for resampled audio
                stream_callback=None
            )

            # Store output channels for conversion
            self.output_channels = output_channels

            self.is_playing = True
            logger.info(f"🔊 Audio speaker started for session {self.session_id}")

            # Start background playback task
            self.playback_task = asyncio.create_task(self._playback_loop())

        except Exception as e:
            logger.error(f"Failed to start audio speaker: {e}")
            raise

    def _resample_audio(self, audio_data: bytes) -> bytes:
        """Resample audio from 16kHz to 44.1kHz using scipy"""
        # Convert bytes to numpy array (16-bit PCM)
        samples_16k = np.frombuffer(audio_data, dtype=np.int16)

        # Resample from INPUT_SAMPLE_RATE to OUTPUT_SAMPLE_RATE
        num_output_samples = int(len(samples_16k) * RESAMPLE_RATIO)
        samples_44k = signal.resample(samples_16k, num_output_samples)

        # Convert back to int16 and ensure values are in range
        samples_44k = np.clip(samples_44k, -32768, 32767).astype(np.int16)

        logger.debug(f"Resampled {len(samples_16k)} samples @ {INPUT_SAMPLE_RATE}Hz → {len(samples_44k)} samples @ {OUTPUT_SAMPLE_RATE}Hz")
        return samples_44k.tobytes()

    def _convert_mono_to_stereo(self, audio_data: bytes) -> bytes:
        """Convert mono audio to stereo by duplicating the channel if needed"""
        if self.output_channels == 1:
            return audio_data

        # Convert bytes to numpy array
        mono_samples = np.frombuffer(audio_data, dtype=np.int16)

        # Duplicate mono to stereo
        stereo_samples = np.repeat(mono_samples, self.output_channels)

        logger.debug(f"Converted {len(mono_samples)} mono samples → {len(stereo_samples)} stereo samples")
        return stereo_samples.tobytes()

    async def _playback_loop(self):
        """Background task for continuous audio playback"""
        logger.debug(f"Starting playback loop for session {self.session_id}")
        logger.debug(f"Converting mono → {self.output_channels} channels")

        while self.is_playing:
            try:
                # Wait for audio data with timeout
                audio_data = await asyncio.wait_for(self.buffer.get(), timeout=0.1)

                # Step 1: Resample from 16kHz to 44.1kHz
                resampled_data = self._resample_audio(audio_data)

                # Step 2: Convert mono to stereo if needed
                output_data = self._convert_mono_to_stereo(resampled_data)

                # Write to stream (blocking but in async task so it's OK)
                if self.stream and not self.stream.is_stopped():
                    try:
                        logger.debug(f"📢 About to write {len(output_data)} bytes to PyAudio stream")
                        self.stream.write(output_data)
                        logger.debug(f"✅ Successfully wrote {len(output_data)} bytes to PyAudio stream")

                        self.total_bytes_played += len(output_data)
                        self.total_frames_played += 1

                        # Log every 10 frames to see if it's working
                        if self.total_frames_played % 10 == 0:
                            queue_size = self.buffer.qsize()
                            logger.info(
                                f"🔊 Playing frame #{self.total_frames_played} "
                                f"({self.total_bytes_played} bytes, queue: {queue_size})"
                            )
                    except Exception as e:
                        logger.error(f"❌ PyAudio stream.write() FAILED: {type(e).__name__}: {e}")
                        logger.error(f"   Data size: {len(output_data)} bytes")
                        logger.error(f"   Stream info: stopped={self.stream.is_stopped()}, active={self.stream.is_active()}")
                        import traceback
                        logger.error(f"   Traceback: {traceback.format_exc()}")
                else:
                    logger.warning(f"⚠️ Stream not available for playback (stream exists: {self.stream is not None})")

            except asyncio.TimeoutError:
                # No data available - this is OK, just continue
                self.underrun_count += 1
                if self.underrun_count % 50 == 0:
                    logger.debug(f"Audio underrun #{self.underrun_count} (waiting for data)")
                continue

            except Exception as e:
                logger.error(f"Error in playback loop: {e}")
                import traceback
                traceback.print_exc()
                await asyncio.sleep(0.01)  # Brief pause on error

    async def play_audio(self, audio_data: bytes):
        """Queue audio data for playback"""
        if not self.is_playing:
            logger.warning(f"Audio stream not active for session {self.session_id}")
            return

        try:
            logger.debug(f"🎵 Queuing {len(audio_data)} bytes to playback buffer (current size: {self.buffer.qsize()})")

            # Try to add to buffer without blocking
            if self.buffer.full():
                # Buffer is full - drop oldest frame to prevent overflow
                try:
                    self.buffer.get_nowait()
                    self.overrun_count += 1
                    if self.overrun_count % 10 == 0:
                        logger.warning(f"Audio buffer overrun #{self.overrun_count} - dropping frames")
                except asyncio.QueueEmpty:
                    pass

            # Add new frame to buffer
            try:
                self.buffer.put_nowait(audio_data)  # Use put_nowait instead of await put
                logger.debug(f"✓ Audio queued successfully (buffer size now: {self.buffer.qsize()})")
            except asyncio.QueueFull:
                logger.warning(f"Buffer full, couldn't queue audio")

        except Exception as e:
            logger.error(f"Error queuing audio: {e}")
            import traceback
            traceback.print_exc()

    async def stop(self):
        """Stop audio output stream"""
        self.is_playing = False

        # Wait for playback task to finish
        if self.playback_task:
            try:
                await asyncio.wait_for(self.playback_task, timeout=2.0)
            except asyncio.TimeoutError:
                logger.warning("Playback task didn't finish in time")
            except Exception as e:
                logger.debug(f"Playback task error on stop: {e}")

        if self.stream:
            try:
                self.stream.stop_stream()
                self.stream.close()
            except Exception as e:
                logger.error(f"Error closing audio stream: {e}")

        try:
            self.audio.terminate()
        except Exception as e:
            logger.error(f"Error terminating PyAudio: {e}")

        logger.info(
            f"🔊 Audio speaker stopped for session {self.session_id} "
            f"(played {self.total_frames_played} frames, {self.total_bytes_played} bytes, "
            f"underruns: {self.underrun_count}, overruns: {self.overrun_count})"
        )


class RustPBXPipecatServer:
    """Pipecat server specifically designed for RustPBX raw audio integration"""

    def __init__(self, host: str = "localhost", port: int = 8765):
        self.host = host
        self.port = port
        self.active_sessions: Dict[str, Dict] = {}
        self.enable_speaker_playback = True  # Enable/disable speaker playback
        
    async def create_ai_services(self):
        """Create AI services for processing (optional - for audio streaming, not required)"""

        # For basic audio streaming to speaker, AI services are optional
        # Try to import and create them if available
        try:
            from pipecat.audio.vad.silero import SileroVADAnalyzer
            from pipecat.services.deepgram import DeepgramSTTService, DeepgramTTSService
            from pipecat.services.openai import OpenAILLMService
            from pipecat.vad.vad_analyzer import VADParams

            # Get API keys from environment
            deepgram_api_key = os.getenv("DEEPGRAM_API_KEY")
            openai_api_key = os.getenv("OPENAI_API_KEY")

            if not deepgram_api_key or not openai_api_key:
                logger.info("AI services disabled (no API keys) - audio streaming only mode")
                return {'stt': None, 'llm': None, 'tts': None, 'vad': None}

            # Create services
            stt = DeepgramSTTService(
                api_key=deepgram_api_key,
                model="nova-2",
                language="en-US",
                sample_rate=16000,
                channels=1,
                interim_results=True,
                smart_format=True,
                endpointing=300,
            )

            llm = OpenAILLMService(
                api_key=openai_api_key,
                model="gpt-4o-mini",
                max_tokens=150,
                temperature=0.7,
            )

            tts = DeepgramTTSService(
                api_key=deepgram_api_key,
                voice="aura-asteria-en",
                sample_rate=16000,
                encoding="linear16",
            )

            vad = SileroVADAnalyzer(
                params=VADParams(
                    stop_secs=0.5,
                    start_secs=0.2,
                    min_volume=0.6,
                )
            )

            logger.info("✓ AI services initialized successfully")
            return {'stt': stt, 'llm': llm, 'tts': tts, 'vad': vad}

        except ImportError as e:
            logger.info(f"AI services disabled ({e}) - audio streaming only mode")
            logger.info("  To enable: pip install 'pipecat-ai[silero,deepgram,openai]'")
            return {'stt': None, 'llm': None, 'tts': None, 'vad': None}
        except Exception as e:
            logger.warning(f"Failed to initialize AI services: {e}")
            logger.info("Continuing in audio streaming only mode")
            return {'stt': None, 'llm': None, 'tts': None, 'vad': None}
        
    async def process_audio_chunk(self, audio_data: bytes, session_id: str) -> Optional[bytes]:
        """Process a single audio chunk - play on speaker and forward to AI pipeline"""

        try:
            session = self.active_sessions.get(session_id)
            if not session:
                logger.warning(f"Session {session_id} not found")
                return None

            services = session['services']
            speaker = session.get('speaker')

            # Convert bytes to numpy array (16-bit PCM) for analysis
            audio_samples = np.frombuffer(audio_data, dtype=np.int16)

            # Calculate audio level for debugging
            if len(audio_samples) > 0:
                max_amplitude = np.max(np.abs(audio_samples))
                avg_amplitude = np.mean(np.abs(audio_samples))
            else:
                max_amplitude = 0
                avg_amplitude = 0

            # Update session stats
            session['total_audio_bytes'] = session.get('total_audio_bytes', 0) + len(audio_data)
            session['total_audio_chunks'] = session.get('total_audio_chunks', 0) + 1
            chunk_num = session['total_audio_chunks']

            # Log with audio levels (every 50 chunks)
            if chunk_num % 50 == 0:
                logger.info(
                    f"📥 Chunk #{chunk_num}: {len(audio_data)} bytes, "
                    f"{len(audio_samples)} samples, max_amp: {max_amplitude}, avg_amp: {avg_amplitude:.0f}"
                )

            # CRITICAL: Play audio on speaker if enabled
            if self.enable_speaker_playback and speaker:
                logger.debug(f"Sending audio to speaker (chunk #{chunk_num})")
                try:
                    await speaker.play_audio(audio_data)
                    logger.debug(f"Audio sent to speaker successfully (chunk #{chunk_num})")
                except Exception as e:
                    logger.error(f"FAILED to send audio to speaker: {e}")
                    import traceback
                    traceback.print_exc()
            else:
                if not self.enable_speaker_playback:
                    logger.warning(f"Speaker playback DISABLED (chunk #{chunk_num})")
                if not speaker:
                    logger.error(f"NO SPEAKER OBJECT (chunk #{chunk_num})")

            # Send periodic acknowledgment (every 50 chunks)
            if chunk_num % 50 == 0:
                await self.send_event_to_rustpbx(session_id, {
                    "type": "metrics",
                    "key": "audio_received",
                    "duration": chunk_num,
                    "timestamp": int(time.time() * 1000)
                })

        except Exception as e:
            logger.error(f"Error processing audio chunk for session {session_id}: {e}")
            import traceback
            traceback.print_exc()

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

        speaker = None

        try:
            # Create AI services for this session
            services = await self.create_ai_services()

            # Create and start audio speaker
            logger.info(f"🔊 Initializing speaker playback (enabled: {self.enable_speaker_playback})")
            if self.enable_speaker_playback:
                try:
                    speaker = AudioSpeakerPlayer(session_id)
                    logger.info(f"🔊 AudioSpeakerPlayer created, calling start()...")
                    await speaker.start()
                    logger.info(f"🔊 AudioSpeakerPlayer started successfully!")
                except Exception as e:
                    logger.error(f"❌ FAILED to start speaker: {e}")
                    import traceback
                    traceback.print_exc()
                    speaker = None
            else:
                logger.warning(f"⚠️ Speaker playback DISABLED for session {session_id}")
                speaker = None

            # Store session data
            self.active_sessions[session_id] = {
                'websocket': websocket,
                'services': services,
                'speaker': speaker,
                'started_at': time.time(),
                'total_audio_bytes': 0,
                'total_audio_chunks': 0,
            }

            # Send ready message to RustPBX
            await self.send_event_to_rustpbx(session_id, {
                "type": "connected",
                "server": "RustPBX-Pipecat-Speaker",
                "version": "2.0.0",
                "speaker_enabled": self.enable_speaker_playback,
                "timestamp": int(time.time() * 1000)
            })

            logger.info(f"✅ Session {session_id} ready for audio processing and playback")

            # Listen for audio data from RustPBX
            async for message in websocket:
                try:
                    if isinstance(message, bytes):
                        # Raw audio data from RustPBX
                        logger.debug(f"Received {len(message)} bytes of audio data from RustPBX")

                        # Process the audio chunk (includes speaker playback)
                        await self.process_audio_chunk(message, session_id)

                    elif isinstance(message, str):
                        # JSON message from RustPBX
                        try:
                            data = json.loads(message)
                            logger.debug(f"Received JSON message from RustPBX: {data}")

                            # Handle different message types
                            msg_type = data.get("type", data.get("command"))

                            if msg_type == "ping":
                                await self.send_event_to_rustpbx(session_id, {
                                    "type": "pong",
                                    "timestamp": data.get("timestamp", int(time.time() * 1000))
                                })

                            elif msg_type == "configure":
                                logger.info(f"Configuration received for session {session_id}: {data}")
                                await self.send_event_to_rustpbx(session_id, {
                                    "type": "configured",
                                    "call_id": data.get("room_id", session_id),
                                    "status": "configured",
                                    "timestamp": int(time.time() * 1000)
                                })

                            elif msg_type == "disconnect":
                                logger.info(f"Disconnect requested for session {session_id}")
                                break

                        except json.JSONDecodeError:
                            logger.warning(f"Invalid JSON message from RustPBX: {message}")

                except Exception as e:
                    logger.error(f"Error processing message in session {session_id}: {e}")
                    break

        except Exception as e:
            logger.error(f"Error in session {session_id}: {e}")
            raise
        finally:
            # Stop speaker
            if speaker:
                await speaker.stop()

            # Clean up session
            if session_id in self.active_sessions:
                session = self.active_sessions[session_id]
                logger.info(
                    f"📊 Session stats - Chunks: {session.get('total_audio_chunks', 0)}, "
                    f"Bytes: {session.get('total_audio_bytes', 0)}, "
                    f"Duration: {time.time() - session['started_at']:.2f}s"
                )
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
            # Only log errors that aren't just invalid HTTP requests
            error_msg = str(e)
            if "InvalidUpgrade" in error_msg or "invalid Connection header" in error_msg:
                logger.debug(f"Ignored non-WebSocket connection attempt: {e}")
            else:
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
            logger.info("ℹ️  Note: 'InvalidUpgrade' errors are harmless (browsers/HTTP clients)")

            # Keep server running
            await asyncio.Future()  # Run forever


async def main():
    """Main entry point"""

    # Check environment variables (optional for basic audio streaming)
    deepgram_key = os.getenv("DEEPGRAM_API_KEY")
    openai_key = os.getenv("OPENAI_API_KEY")

    if not deepgram_key or not openai_key:
        logger.warning("⚠️  API keys not set - running in AUDIO STREAMING ONLY mode")
        logger.warning("   To enable AI processing, set:")
        logger.warning("   - DEEPGRAM_API_KEY (for speech-to-text)")
        logger.warning("   - OPENAI_API_KEY (for LLM)")
        logger.warning("")
        logger.info("✅ Audio streaming to speaker will still work!")
        logger.info("")

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