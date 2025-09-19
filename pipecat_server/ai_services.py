"""
AI service integrations for speech processing pipeline
"""

import asyncio
import base64
import json
import time
from typing import Optional, Dict, Any
from loguru import logger

# AI service imports
try:
    import openai
    from openai import AsyncOpenAI
except ImportError:
    openai = None
    AsyncOpenAI = None

try:
    from deepgram import DeepgramClient, PrerecordedOptions, SpeakOptions
except ImportError:
    DeepgramClient = None
    PrerecordedOptions = None
    SpeakOptions = None

from config import Settings
from models import MessageType, AudioFormat


class STTService:
    """Speech-to-Text service using Deepgram"""
    
    def __init__(self, settings: Settings):
        self.settings = settings
        self.client = None
        
    async def initialize(self):
        """Initialize the STT service"""
        if not DeepgramClient or not self.settings.deepgram_api_key:
            logger.warning("⚠️ Deepgram not available - STT service disabled")
            return
            
        try:
            self.client = DeepgramClient(self.settings.deepgram_api_key)
            logger.info("✅ Deepgram STT service initialized")
        except Exception as e:
            logger.error(f"❌ Failed to initialize Deepgram STT: {e}")
            
    async def transcribe(self, audio_data: bytes, format: str = "linear16") -> Optional[str]:
        """Transcribe audio data to text"""
        if not self.client:
            return None
            
        try:
            options = PrerecordedOptions(
                model=self.settings.stt_model,
                language=self.settings.stt_language,
                smart_format=True,
                punctuate=True,
            )
            
            response = await asyncio.to_thread(
                lambda: self.client.listen.prerecorded.v("1").transcribe_file(
                    {"buffer": audio_data, "mimetype": f"audio/{format}"},
                    options
                )
            )
            
            if response.results and response.results.channels:
                transcript = response.results.channels[0].alternatives[0].transcript
                logger.debug(f"🎤 STT: {transcript[:50]}...")
                return transcript.strip()
                
        except Exception as e:
            logger.error(f"❌ STT transcription error: {e}")
            
        return None


class LLMService:
    """Language Model service using OpenAI"""
    
    def __init__(self, settings: Settings):
        self.settings = settings
        self.client = None
        self.conversation_history: Dict[str, list] = {}
        
    async def initialize(self):
        """Initialize the LLM service"""
        if not AsyncOpenAI or not self.settings.openai_api_key:
            logger.warning("⚠️ OpenAI not available - LLM service disabled")
            return
            
        try:
            self.client = AsyncOpenAI(api_key=self.settings.openai_api_key)
            logger.info("✅ OpenAI LLM service initialized")
        except Exception as e:
            logger.error(f"❌ Failed to initialize OpenAI LLM: {e}")
            
    async def generate_response(self, text: str, session_id: str = "default", 
                              system_prompt: str = None) -> Optional[str]:
        """Generate a response using the language model"""
        if not self.client:
            return None
            
        try:
            # Initialize conversation history for session
            if session_id not in self.conversation_history:
                self.conversation_history[session_id] = []
                
            # Add system prompt if provided
            messages = []
            if system_prompt:
                messages.append({"role": "system", "content": system_prompt})
            
            # Add conversation history
            messages.extend(self.conversation_history[session_id])
            
            # Add current user message
            messages.append({"role": "user", "content": text})
            
            # Generate response
            response = await self.client.chat.completions.create(
                model=self.settings.llm_model,
                messages=messages,
                max_tokens=self.settings.llm_max_tokens,
                temperature=self.settings.llm_temperature,
                stream=False
            )
            
            if response.choices:
                assistant_message = response.choices[0].message.content
                
                # Update conversation history
                self.conversation_history[session_id].append({"role": "user", "content": text})
                self.conversation_history[session_id].append({"role": "assistant", "content": assistant_message})
                
                # Keep only last 10 messages to manage context length
                if len(self.conversation_history[session_id]) > 10:
                    self.conversation_history[session_id] = self.conversation_history[session_id][-10:]
                
                logger.debug(f"🤖 LLM: {assistant_message[:50]}...")
                return assistant_message
                
        except Exception as e:
            logger.error(f"❌ LLM generation error: {e}")
            
        return None
        
    def clear_history(self, session_id: str):
        """Clear conversation history for a session"""
        if session_id in self.conversation_history:
            del self.conversation_history[session_id]
            logger.debug(f"🧹 Cleared conversation history for session: {session_id}")


class TTSService:
    """Text-to-Speech service using Deepgram"""
    
    def __init__(self, settings: Settings):
        self.settings = settings
        self.client = None
        
    async def initialize(self):
        """Initialize the TTS service"""
        if not DeepgramClient or not self.settings.deepgram_api_key:
            logger.warning("⚠️ Deepgram not available - TTS service disabled")
            return
            
        try:
            self.client = DeepgramClient(self.settings.deepgram_api_key)
            logger.info("✅ Deepgram TTS service initialized")
        except Exception as e:
            logger.error(f"❌ Failed to initialize Deepgram TTS: {e}")
            
    async def synthesize(self, text: str, format: str = "linear16") -> Optional[bytes]:
        """Synthesize text to speech"""
        if not self.client:
            return None
            
        try:
            options = SpeakOptions(
                model=self.settings.tts_model,
                encoding=format,
                sample_rate=self.settings.sample_rate,
            )
            
            response = await asyncio.to_thread(
                lambda: self.client.speak.v("1").save(
                    text,
                    options
                )
            )
            
            if response:
                logger.debug(f"🔊 TTS: Generated audio for text ({len(text)} chars)")
                return response
                
        except Exception as e:
            logger.error(f"❌ TTS synthesis error: {e}")
            
        return None


class AIProcessor:
    """Main AI processing pipeline coordinator"""
    
    def __init__(self, settings: Settings):
        self.settings = settings
        self.stt = STTService(settings)
        self.llm = LLMService(settings)
        self.tts = TTSService(settings)
        
        # Processing statistics
        self.stats = {
            "messages_processed": 0,
            "audio_frames_processed": 0,
            "errors": 0,
            "start_time": time.time()
        }
        
    async def initialize(self):
        """Initialize all AI services"""
        logger.info("🤖 Initializing AI processing pipeline")
        await asyncio.gather(
            self.stt.initialize(),
            self.llm.initialize(),
            self.tts.initialize()
        )
        logger.info("✅ AI processing pipeline ready")
        
    async def cleanup(self):
        """Clean up AI service resources"""
        logger.info("🧹 Cleaning up AI services")
        # Clear all conversation histories
        self.llm.conversation_history.clear()
        
    async def process_message(self, message: dict, websocket_manager=None) -> Optional[dict]:
        """Process incoming WebSocket message through AI pipeline"""
        try:
            self.stats["messages_processed"] += 1
            message_type = message.get("type", message.get("command"))
            
            if message_type == MessageType.CONFIGURE:
                return await self._handle_configure(message)
            elif message_type == MessageType.AUDIO:
                return await self._handle_audio(message, websocket_manager)
            elif message_type == MessageType.TEXT:
                return await self._handle_text(message)
            elif message_type == MessageType.PING:
                return {"type": MessageType.PONG, "timestamp": int(time.time() * 1000)}
            else:
                logger.warning(f"⚠️ Unknown message type: {message_type}")
                return None
                
        except Exception as e:
            self.stats["errors"] += 1
            logger.error(f"❌ Error processing message: {e}")
            return {
                "type": MessageType.ERROR,
                "error": str(e),
                "timestamp": time.time()
            }
            
    async def _handle_configure(self, message: dict) -> dict:
        """Handle configuration messages"""
        config = message.get("config", {})
        session_id = message.get("session_id", "default")
        
        # Clear conversation history if requested
        if config.get("clear_history"):
            self.llm.clear_history(session_id)
            
        logger.info(f"🔧 Configuration updated for session: {session_id}")
        return {
            "type": MessageType.STATUS,
            "status": "configured",
            "timestamp": time.time()
        }
        
    async def _handle_audio(self, message: dict, websocket_manager=None) -> Optional[dict]:
        """Handle audio messages - STT -> LLM -> TTS pipeline"""
        try:
            self.stats["audio_frames_processed"] += 1
            
            # Handle RustPBX audio format
            audio_data = None
            session_id = message.get("session_id", message.get("call_id", "default"))
            
            # Check for different audio data formats
            if "audio_data" in message:
                # RustPBX PipecatAudioFrame format
                audio_raw = message.get("audio_data")
                if isinstance(audio_raw, str):
                    # Base64 encoded audio
                    audio_data = base64.b64decode(audio_raw)
                elif isinstance(audio_raw, list):
                    # Raw byte array from RustPBX
                    audio_data = bytes(audio_raw)
                else:
                    audio_data = audio_raw
            elif "data" in message:
                # Alternative audio data field
                data = message.get("data")
                if isinstance(data, str):
                    audio_data = base64.b64decode(data)
                elif isinstance(data, list):
                    # Convert list of integers to bytes
                    audio_data = bytes(data)
            elif "samples" in message:
                # Audio samples format
                samples = message.get("samples", [])
                if samples:
                    audio_data = bytes(samples)
            
            if not audio_data:
                logger.warning("⚠️ No audio data found in message")
                return None
                
            # Log audio reception
            audio_length = len(audio_data)
            sample_rate = message.get("sample_rate", 16000)
            logger.info(f"🎵 Received audio: {audio_length} bytes, {sample_rate}Hz from session {session_id}")
                
            format = message.get("format", "linear16")
            
            # Step 1: Speech-to-Text
            # Notify dashboard of processing start
            if websocket_manager:
                await websocket_manager.broadcast_message({
                    "type": "transcription",
                    "is_final": False,
                    "text": "Processing audio...",
                    "timestamp": int(time.time() * 1000)
                })
            
            transcript = await self.stt.transcribe(audio_data, format)
            if not transcript:
                if websocket_manager:
                    await websocket_manager.broadcast_message({
                        "type": "error",
                        "message": "Speech recognition failed",
                        "timestamp": int(time.time() * 1000)
                    })
                return None
                
            logger.info(f"🎤 STT: {transcript}")
            
            # Notify dashboard of final transcription
            if websocket_manager:
                await websocket_manager.broadcast_message({
                    "type": "transcription",
                    "is_final": True,
                    "text": transcript,
                    "timestamp": int(time.time() * 1000)
                })
            
            # Step 2: Generate LLM response
            # Notify dashboard of LLM processing start
            if websocket_manager:
                await websocket_manager.broadcast_message({
                    "type": "llm_response",
                    "is_complete": False,
                    "text": "Generating response...",
                    "timestamp": int(time.time() * 1000)
                })
            
            system_prompt = message.get("system_prompt", "You are a helpful AI assistant in a voice conversation. Respond naturally and conversationally. Keep responses brief but informative.")
            llm_response = await self.llm.generate_response(transcript, session_id, system_prompt)
            if not llm_response:
                if websocket_manager:
                    await websocket_manager.broadcast_message({
                        "type": "error",
                        "message": "Language model response failed",
                        "timestamp": int(time.time() * 1000)
                    })
                return None
                
            logger.info(f"🤖 LLM: {llm_response}")
            
            # Notify dashboard of final LLM response
            if websocket_manager:
                await websocket_manager.broadcast_message({
                    "type": "llm_response",
                    "is_complete": True,
                    "text": llm_response,
                    "timestamp": int(time.time() * 1000)
                })
            
            # Step 3: Text-to-Speech
            # Notify dashboard of TTS start
            if websocket_manager:
                await websocket_manager.broadcast_message({
                    "type": "tts_started",
                    "text": llm_response,
                    "timestamp": int(time.time() * 1000)
                })
            
            audio_response = await self.tts.synthesize(llm_response, format)
            if not audio_response:
                if websocket_manager:
                    await websocket_manager.broadcast_message({
                        "type": "error",
                        "message": "Text-to-speech synthesis failed",
                        "timestamp": int(time.time() * 1000)
                    })
                # Return text response if TTS fails
                return {
                    "type": "llm_response",
                    "text": llm_response,
                    "is_complete": True,
                    "timestamp": int(time.time() * 1000)
                }
                
            logger.info(f"🔊 TTS: Generated audio response")
            
            # Notify dashboard of TTS completion
            if websocket_manager:
                await websocket_manager.broadcast_message({
                    "type": "tts_completed",
                    "text": llm_response,
                    "timestamp": int(time.time() * 1000)
                })
            
            # Return audio response in RustPBX format
            return {
                "type": "audio",
                "audio_data": list(audio_response),  # Convert bytes to list for JSON serialization
                "sample_rate": self.settings.sample_rate,
                "channels": self.settings.channels,
                "frame_id": f"{session_id}_{int(time.time() * 1000)}",
                "timestamp": int(time.time() * 1000)
            }
            
        except Exception as e:
            logger.error(f"❌ Audio processing error: {e}")
            session_id = message.get("session_id", message.get("call_id", "default"))
            return {
                "type": "error",
                "message": str(e),
                "timestamp": int(time.time() * 1000)
            }
            
    async def _handle_text(self, message: dict) -> Optional[dict]:
        """Handle text messages - LLM only"""
        try:
            text = message.get("text", "")
            if not text:
                return None
                
            session_id = message.get("session_id", "default")
            system_prompt = message.get("system_prompt", "You are a helpful AI assistant.")
            
            # Generate LLM response
            llm_response = await self.llm.generate_response(text, session_id, system_prompt)
            if not llm_response:
                return None
                
            return {
                "type": MessageType.TEXT,
                "text": llm_response,
                "is_final": True,
                "session_id": session_id,
                "timestamp": time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ Text processing error: {e}")
            return None
            
    def get_stats(self) -> dict:
        """Get processing statistics"""
        uptime = time.time() - self.stats["start_time"]
        return {
            **self.stats,
            "uptime_seconds": uptime,
            "messages_per_second": self.stats["messages_processed"] / max(uptime, 1),
            "error_rate": self.stats["errors"] / max(self.stats["messages_processed"], 1)
        }