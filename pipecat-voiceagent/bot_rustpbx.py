#!/usr/bin/env python3
"""
Pipecat Voice Agent Bot for RustPBX Integration

This bot processes audio through the Pipecat pipeline without SmallWebRTC.
It receives raw audio from RustPBX via WebSocket and returns processed audio.

Pipeline: Audio Input -> STT -> LLM -> TTS -> Audio Output
"""

import asyncio
import os
import struct
import time
from typing import Callable, Awaitable

from dotenv import load_dotenv
from loguru import logger
from pipecat.audio.vad.silero import SileroVADAnalyzer
from pipecat.audio.vad.vad_analyzer import VADParams
from pipecat.frames.frames import (
    Frame,
    AudioRawFrame,
    InputAudioRawFrame,
    OutputAudioRawFrame,
    LLMFullResponseEndFrame,
    LLMFullResponseStartFrame,
    LLMMessagesFrame,
    StartInterruptionFrame,
    TextFrame,
    TranscriptionFrame,
    TTSAudioRawFrame,
    TTSStartedFrame,
    TTSStoppedFrame,
)
from pipecat.pipeline.pipeline import Pipeline
from pipecat.pipeline.runner import PipelineRunner
from pipecat.pipeline.task import PipelineParams, PipelineTask
from pipecat.processors.aggregators.openai_llm_context import OpenAILLMContext
from pipecat.processors.frame_processor import FrameDirection, FrameProcessor
from pipecat.services.deepgram import DeepgramSTTService  # Using Deepgram for STT
from pipecat.services.cartesia.tts import CartesiaTTSService  # Using Cartesia for TTS
from pipecat.services.openai.llm import OpenAILLMService

from rustpbx_serializer import RustPBXFrameSerializer

load_dotenv(override=True)

# Audio configuration - must match RustPBX (16kHz, mono, 16-bit PCM)
SAMPLE_RATE = 16000
CHANNELS = 1
SAMPLE_WIDTH = 2  # 16-bit = 2 bytes

SYSTEM_INSTRUCTION = """You are a friendly, helpful AI assistant.

Your goal is to demonstrate your capabilities in a succinct way.

Your output will be converted to audio so don't include special characters in your answers.

Respond to what the user said in a creative and helpful way. Keep your responses brief. One or two sentences at most.
"""


class AudioInputProcessor(FrameProcessor):
    """Source processor that generates audio frames from the queue using RustPBX serializer"""

    def __init__(self, audio_queue: asyncio.Queue, serializer):
        super().__init__()
        self.audio_queue = audio_queue
        self.serializer = serializer
        self._running = False

    async def process_frame(self, frame: Frame, direction: FrameDirection):
        """
        This processor acts as a SOURCE - it generates frames rather than processing them.
        When StartFrame arrives, we start generating audio frames from the queue.
        """
        from pipecat.frames.frames import StartFrame

        # Handle StartFrame to mark as started
        if isinstance(frame, StartFrame):
            logger.info("🎬 AudioInputProcessor: Received StartFrame, starting audio generation")
            self._running = True
            # Start generating audio frames in background
            asyncio.create_task(self._generate_audio_frames())

        # Pass the frame through
        await super().process_frame(frame, direction)

    async def _generate_audio_frames(self):
        """Generate audio frames from the queue and push downstream"""
        frame_count = 0
        logger.info("🚰 Audio generation: Started")

        while self._running:
            try:
                # Get audio data from queue
                audio_data = await asyncio.wait_for(
                    self.audio_queue.get(),
                    timeout=1.0
                )

                frame_count += 1

                # Log first few frames
                if frame_count <= 5:
                    logger.info(f"🎤 Generator: Got audio from queue frame #{frame_count}, {len(audio_data)} bytes")

                # Use serializer to deserialize RustPBX audio data to InputAudioRawFrame
                audio_frame = await self.serializer.deserialize(audio_data)

                if not audio_frame:
                    logger.warning(f"⚠️  Failed to deserialize audio frame #{frame_count}")
                    continue

                # Pipecat's turn tracking requires frames to have an id
                audio_frame.id = frame_count

                # Log first few frames being pushed
                if frame_count <= 5:
                    num_samples = len(audio_data) // SAMPLE_WIDTH
                    logger.info(f"⚡ Generator: Pushing InputAudioRawFrame #{frame_count} downstream ({num_samples} samples)")

                # Push frame downstream to next processor (STT)
                await self.push_frame(audio_frame)

                # Log periodically
                if frame_count % 100 == 0:
                    logger.info(f"🎤 Generator: Pushed {frame_count} audio frames")

            except asyncio.TimeoutError:
                # No audio available, continue waiting
                continue
            except Exception as e:
                logger.error(f"❌ Generator error: {e}", exc_info=True)
                break

        logger.info("🚰 Audio generation: Stopped")

    def stop(self):
        """Stop audio generation"""
        logger.info("🛑 Stopping audio generator")
        self._running = False


class AudioOutputProcessor(FrameProcessor):
    """Sends processed audio back to RustPBX"""

    def __init__(self, send_callback: Callable[[dict], Awaitable[None]], serializer):
        super().__init__()
        self.send_callback = send_callback
        self.serializer = serializer
        self.audio_buffer = bytearray()
        self.current_text = ""

    async def process_frame(self, frame: Frame, direction: FrameDirection):
        """Process frames and send appropriate responses to RustPBX"""

        # Let parent class handle framework frames (StartFrame, etc.)
        await super().process_frame(frame, direction)

        # Handle transcription frames
        if isinstance(frame, TranscriptionFrame):
            is_final = hasattr(frame, "final") and frame.final
            await self.send_callback({
                "type": "transcription",
                "text": frame.text,
                "is_final": is_final,
                "timestamp": int(time.time() * 1000)
            })
            logger.info(f"📝 Transcription ({'final' if is_final else 'partial'}): {frame.text}")

        # Handle LLM response start
        elif isinstance(frame, LLMFullResponseStartFrame):
            self.current_text = ""
            logger.info("LLM response started")

        # Handle LLM text fragments
        elif isinstance(frame, TextFrame):
            self.current_text += frame.text
            await self.send_callback({
                "type": "llm_response",
                "text": frame.text,
                "is_complete": False,
                "timestamp": int(time.time() * 1000)
            })
            logger.info(f"LLM response: {frame.text}")

        # Handle LLM response end
        elif isinstance(frame, LLMFullResponseEndFrame):
            if self.current_text:
                await self.send_callback({
                    "type": "llm_response",
                    "text": self.current_text,
                    "is_complete": True,
                    "timestamp": int(time.time() * 1000)
                })
                logger.info(f"LLM response complete: {self.current_text}")
                self.current_text = ""

        # Handle TTS start
        elif isinstance(frame, TTSStartedFrame):
            await self.send_callback({
                "type": "tts_started",
                "text": getattr(frame, "text", ""),
                "timestamp": int(time.time() * 1000)
            })
            logger.info("TTS started")

        # Handle TTS audio
        elif isinstance(frame, TTSAudioRawFrame):
            # Accumulate audio data
            self.audio_buffer.extend(frame.audio)
            logger.info(f"🔊 Received TTS audio frame: {len(frame.audio)} bytes (buffer now {len(self.audio_buffer)} bytes)")

            # Send in chunks to avoid large messages
            # RustPBX expects 16-bit PCM at 16kHz
            chunk_size = SAMPLE_RATE * SAMPLE_WIDTH  # 1 second of audio

            while len(self.audio_buffer) >= chunk_size:
                chunk = bytes(self.audio_buffer[:chunk_size])
                self.audio_buffer = self.audio_buffer[chunk_size:]

                logger.info(f"📤 Sending audio chunk to RustPBX: {len(chunk)} bytes")
                await self.send_callback({
                    "type": "audio",
                    "audio_data": list(chunk),  # Convert to list for JSON serialization
                    "sample_rate": SAMPLE_RATE,
                    "channels": CHANNELS,
                    "frame_id": f"audio_{int(time.time() * 1000)}"
                })

                logger.debug(f"Sent audio chunk: {len(chunk)} bytes")

        # Handle TTS stop
        elif isinstance(frame, TTSStoppedFrame):
            # Send any remaining audio
            if len(self.audio_buffer) > 0:
                chunk = bytes(self.audio_buffer)
                self.audio_buffer.clear()

                await self.send_callback({
                    "type": "audio",
                    "audio_data": list(chunk),
                    "sample_rate": SAMPLE_RATE,
                    "channels": CHANNELS,
                    "frame_id": f"audio_{int(time.time() * 1000)}"
                })

                logger.debug(f"Sent final audio chunk: {len(chunk)} bytes")

            await self.send_callback({
                "type": "tts_completed",
                "text": getattr(frame, "text", ""),
                "timestamp": int(time.time() * 1000)
            })
            logger.info("TTS completed")

        # Note: super().process_frame() already pushed the frame along the pipeline


async def create_bot_pipeline(
    audio_queue: asyncio.Queue,
    send_callback: Callable[[dict], Awaitable[None]],
    connection_id: str
):
    """Create and run the Pipecat bot pipeline"""

    logger.info(f"🚀 Creating bot pipeline for connection {connection_id}")
    logger.info(f"📊 Audio queue status: {audio_queue.qsize()} items in queue")

    try:
        # Configure VAD parameters
        vad_params = VADParams(
            confidence=0.8,
            start_secs=0.3,
            stop_secs=1.0,
        )

        # Create pipeline task first (needed for AudioInputProcessor)
        # We'll create the actual pipeline and task below
        task = None

        # Create RustPBX serializer for audio frame conversion (needed by both input and output)
        serializer = RustPBXFrameSerializer(
            sample_rate=SAMPLE_RATE,
            num_channels=CHANNELS
        )
        logger.info(f"🔄 Created RustPBX serializer: {SAMPLE_RATE}Hz, {CHANNELS} channel(s)")

        # Create output processor
        audio_output = AudioOutputProcessor(send_callback, serializer)

        # Create AI services - Using Deepgram for STT (more stable with Pipecat 0.0.82)
        stt = DeepgramSTTService(
            api_key=os.getenv("DEEPGRAM_API_KEY"),
            model="nova-2",  # Deepgram's latest model
            language="en"
        )
        logger.info(f"🎙️  Created DeepgramSTT with model=nova-2")

        llm = OpenAILLMService(
            api_key=os.getenv("OPENAI_API_KEY"),
            model="gpt-4o-mini"
        )

        tts = CartesiaTTSService(
            api_key=os.getenv("CARTESIA_API_KEY"),
            voice_id="79a125e8-cd45-4c13-8a67-188112f4dd22"  # British Lady
        )

        # Create LLM context
        context = OpenAILLMContext(
            [
                {
                    "role": "system",
                    "content": SYSTEM_INSTRUCTION,
                },
                {
                    "role": "user",
                    "content": "Start by greeting the user warmly and introducing yourself.",
                }
            ],
        )
        context_aggregator = llm.create_context_aggregator(context)

        # Create audio input processor (will generate frames from queue)
        audio_input = AudioInputProcessor(audio_queue, serializer)

        # Build pipeline WITH audio_input as the source
        # Remove debug processors to avoid Pipecat 0.0.91 internal issues
        pipeline = Pipeline(
            [
                audio_input,          # SOURCE: Generates audio frames from queue
                stt,                  # Speech-to-Text
                context_aggregator.user(),  # Add to context
                llm,                  # LLM processing
                tts,                  # Text-to-Speech
                audio_output,         # Send back to RustPBX
                context_aggregator.assistant(),  # Add to context
            ]
        )

        # Create pipeline task
        task = PipelineTask(
            pipeline,
            params=PipelineParams(
                enable_metrics=True,
                enable_usage_metrics=True,
            ),
        )

        # Run pipeline
        runner = PipelineRunner(handle_sigint=False)
        logger.info(f"▶️  Starting pipeline runner for connection {connection_id}")

        # Start the pipeline runner - it will send StartFrame which will activate audio generation
        logger.info(f"🏃 Running pipeline task...")
        try:
            await runner.run(task)
            logger.info(f"✅ Pipeline runner completed for connection {connection_id}")
        except Exception as e:
            logger.error(f"❌ Pipeline runner error: {e}", exc_info=True)
        finally:
            # Stop audio input
            logger.info(f"🛑 Stopping audio input")
            audio_input.stop()

        logger.info(f"🏁 Pipeline fully stopped for connection {connection_id}")

    except Exception as e:
        logger.error(f"Error in bot pipeline: {e}", exc_info=True)
        await send_callback({
            "type": "error",
            "message": f"Bot pipeline error: {str(e)}",
            "code": 500,
            "timestamp": int(time.time() * 1000)
        })
        raise


async def run_bot_with_transport(
    websocket,
    connection_id: str
):
    """
    Create and run the Pipecat bot pipeline using RustPBX transport.

    This is the new transport-based approach that follows the FastAPIWebsocketTransport pattern.
    It replaces the manual queue-based audio handling.

    Args:
        websocket: The FastAPI WebSocket connection from RustPBX.
        connection_id: Unique identifier for this connection.
    """
    from rustpbx_transport import RustPBXTransport, RustPBXTransportParams

    logger.info(f"🚀 Creating bot pipeline with transport for connection {connection_id}")

    try:
        # Create RustPBX serializer for audio frame conversion
        serializer = RustPBXFrameSerializer(
            sample_rate=SAMPLE_RATE,
            num_channels=CHANNELS
        )
        logger.info(f"🔄 Created RustPBX serializer: {SAMPLE_RATE}Hz, {CHANNELS} channel(s)")

        # Create transport with serializer
        transport_params = RustPBXTransportParams(
            serializer=serializer,
            audio_in_enabled=True,
            audio_out_enabled=True,
            vad_enabled=True,
            vad_analyzer=SileroVADAnalyzer(),
            vad_audio_passthrough=True,
        )

        transport = RustPBXTransport(
            websocket=websocket,
            params=transport_params,
        )

        # Setup WebSocket client
        await transport._client.setup()

        # Register event handlers
        @transport.event_handler("on_client_connected")
        async def on_client_connected(ws):
            logger.info(f"✅ Client connected: {connection_id}")

        @transport.event_handler("on_client_disconnected")
        async def on_client_disconnected(ws):
            logger.info(f"👋 Client disconnected: {connection_id}")

        # Create AI services
        stt = DeepgramSTTService(
            api_key=os.getenv("DEEPGRAM_API_KEY"),
            model="nova-2",
            language="en"
        )
        logger.info("🎙️  Created DeepgramSTT with model=nova-2")

        llm = OpenAILLMService(
            api_key=os.getenv("OPENAI_API_KEY"),
            model="gpt-4o-mini"
        )

        tts = CartesiaTTSService(
            api_key=os.getenv("CARTESIA_API_KEY"),
            voice_id="79a125e8-cd45-4c13-8a67-188112f4dd22"  # British Lady
        )

        # Create LLM context
        context = OpenAILLMContext(
            [
                {
                    "role": "system",
                    "content": SYSTEM_INSTRUCTION,
                },
                {
                    "role": "user",
                    "content": "Start by greeting the user warmly and introducing yourself.",
                }
            ],
        )
        context_aggregator = llm.create_context_aggregator(context)

        # Build pipeline using transport.input() and transport.output()
        # This follows the FastAPIWebsocketTransport pattern
        pipeline = Pipeline([
            transport.input(),           # Receive audio from RustPBX WebSocket
            stt,                         # Speech-to-text
            context_aggregator.user(),   # Aggregate user messages
            llm,                         # Language model
            tts,                         # Text-to-speech
            transport.output(),          # Send audio back to RustPBX WebSocket
            context_aggregator.assistant(),  # Aggregate assistant messages
        ])

        # Create and run pipeline task
        task = PipelineTask(
            pipeline,
            params=PipelineParams(
                allow_interruptions=True,
                enable_metrics=True,
                enable_usage_metrics=True,
            ),
        )

        logger.info("🏃 Running bot pipeline with transport...")

        # Run the pipeline
        runner = PipelineRunner()
        await runner.run(task)

        logger.info("✅ Bot pipeline completed successfully")

    except Exception as e:
        logger.error(f"❌ Error in bot pipeline with transport: {e}", exc_info=True)
        raise


async def run_simple_bot(
    audio_queue: asyncio.Queue,
    send_callback,
    connection_id: str
):
    """
    Run a simple Pipecat bot pipeline (compatibility wrapper for server_rustpbx.py).

    Args:
        audio_queue: Queue receiving binary audio from RustPBX
        send_callback: Callback to send responses back to RustPBX
        connection_id: Unique connection identifier
    """
    from typing import Callable, Awaitable

    logger.info(f"🚀 Starting simple bot for connection {connection_id}")

    try:
        # Create serializer
        serializer = RustPBXFrameSerializer(
            sample_rate=SAMPLE_RATE,
            num_channels=CHANNELS
        )

        # Create input/output processors
        audio_input = SimpleAudioInput(audio_queue, serializer)
        audio_output = SimpleAudioOutput(send_callback, serializer)
        pipeline_logger = PipelineLogger()

        # Create AI services
        stt = DeepgramSTTService(
            api_key=os.getenv("DEEPGRAM_API_KEY"),
            model="nova-2",
            language="en"
        )
        logger.info("🎙️  Created Deepgram STT")

        llm = OpenAILLMService(
            api_key=os.getenv("OPENAI_API_KEY"),
            model="gpt-4o-mini"
        )
        logger.info("🤖 Created OpenAI LLM")

        tts = CartesiaTTSService(
            api_key=os.getenv("CARTESIA_API_KEY"),
            voice_id="79a125e8-cd45-4c13-8a67-188112f4dd22"  # British Lady
        )
        logger.info("🔊 Created Cartesia TTS")

        # Create LLM context
        context = OpenAILLMContext([
            {
                "role": "system",
                "content": SYSTEM_INSTRUCTION,
            },
            {
                "role": "user",
                "content": "Start by greeting the user warmly and introducing yourself.",
            }
        ])
        context_aggregator = llm.create_context_aggregator(context)

        # Build pipeline with logging
        logger.info("📋 Building pipeline: Audio Input → STT → LLM → TTS → Audio Output")
        pipeline = Pipeline([
            audio_input,                    # Receive audio from queue
            stt,                            # Speech-to-text
            pipeline_logger,                # Log STT/LLM/TTS events
            context_aggregator.user(),      # User context
            llm,                            # Language model
            tts,                            # Text-to-speech
            audio_output,                   # Send audio via callback
            context_aggregator.assistant(), # Assistant context
        ])

        # Create and run task
        task = PipelineTask(
            pipeline,
            params=PipelineParams(
                allow_interruptions=True,
                enable_metrics=True,
                enable_usage_metrics=True,
            ),
        )

        logger.info("🏃 Running pipeline...")
        runner = PipelineRunner()
        await runner.run(task)

        logger.info("✅ Pipeline completed")

    except Exception as e:
        logger.error(f"❌ Error in pipeline: {e}", exc_info=True)
        raise


if __name__ == "__main__":
    # This is primarily used as a module, but can be tested standalone
    logger.info("bot_rustpbx.py - Use server_rustpbx.py to start the server")
