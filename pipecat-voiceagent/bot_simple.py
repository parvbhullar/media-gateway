#!/usr/bin/env python3
"""
Simple Pipecat Bot for RustPBX Integration

This version works with the websockets library server (server_rustpbx.py)
and provides a cleaner integration without FastAPI transport complexity.

Architecture:
    RustPBX WebRTC → WebSocket (binary audio) → Deserialize → Pipeline → Serialize → WebSocket → RustPBX Speaker
"""

import asyncio
import os
from typing import Callable, Awaitable

from dotenv import load_dotenv
from loguru import logger

from pipecat.frames.frames import (
    InputAudioRawFrame,
    OutputAudioRawFrame,
    StartFrame,
    TranscriptionFrame,
    LLMFullResponseEndFrame,
    TextFrame,
)
from pipecat.pipeline.pipeline import Pipeline
from pipecat.pipeline.runner import PipelineRunner
from pipecat.pipeline.task import PipelineParams, PipelineTask
from pipecat.processors.aggregators.openai_llm_context import OpenAILLMContext
from pipecat.processors.frame_processor import FrameDirection, FrameProcessor
from pipecat.services.deepgram.stt import DeepgramSTTService
from pipecat.services.cartesia.tts import CartesiaTTSService
from pipecat.services.openai.llm import OpenAILLMService
from pipecat.transports.base_input import BaseInputTransport
from pipecat.transports.base_transport import TransportParams

from rustpbx_serializer import RustPBXFrameSerializer

load_dotenv(override=True)

# Audio configuration
SAMPLE_RATE = 16000
CHANNELS = 1

SYSTEM_INSTRUCTION = """You are a friendly, helpful AI assistant.

Your goal is to demonstrate your capabilities in a succinct way.

Your output will be converted to audio so don't include special characters in your answers.

Respond to what the user said in a creative and helpful way. Keep your responses brief. One or two sentences at most.
"""


class SimpleAudioInput(BaseInputTransport):
    """Simple audio input that receives from queue and deserializes"""

    def __init__(self, audio_queue: asyncio.Queue, serializer: RustPBXFrameSerializer, **kwargs):
        # Create transport params
        params = TransportParams(
            audio_in_enabled=True,
            audio_in_sample_rate=16000,
        )
        super().__init__(params, **kwargs)
        self.audio_queue = audio_queue
        self.serializer = serializer
        self._running = False
        self._receive_task = None

    async def start(self, frame: StartFrame):
        await super().start(frame)
        self._running = True

        # Start receiving and pushing audio frames
        self._receive_task = asyncio.create_task(self._receive_audio())
        logger.info("✅ SimpleAudioInput started")

    async def stop(self, frame):
        await super().stop(frame)
        self._running = False
        if self._receive_task:
            self._receive_task.cancel()
            try:
                await self._receive_task
            except asyncio.CancelledError:
                pass
        logger.info("⏹️  SimpleAudioInput stopped")

    async def _receive_audio(self):
        """Receive audio from queue and push to pipeline"""
        frame_count = 0
        logger.info("🎤 Starting to receive audio from queue")

        while self._running:
            try:
                # Get audio data from queue
                audio_data = await asyncio.wait_for(self.audio_queue.get(), timeout=1.0)

                # Deserialize to InputAudioRawFrame
                audio_frame = await self.serializer.deserialize(audio_data)

                if audio_frame:
                    frame_count += 1
                    audio_frame.id = frame_count

                    # Log first 10 frames, then every 50th frame
                    if frame_count <= 10 or frame_count % 50 == 0:
                        logger.info(f"🎤 RX Frame #{frame_count}: {len(audio_data)} bytes → Pushing to STT pipeline")

                    # Push to pipeline
                    await self.push_frame(audio_frame)

                    if frame_count % 100 == 0:
                        logger.info(f"📊 Total audio frames processed: {frame_count}")

            except asyncio.TimeoutError:
                continue
            except Exception as e:
                logger.error(f"❌ Error receiving audio: {e}", exc_info=True)
                break

        logger.info(f"🏁 Stopped receiving audio (total frames: {frame_count})")


class PipelineLogger(FrameProcessor):
    """Logs all frames passing through the pipeline for debugging"""

    def __init__(self):
        super().__init__()
        self.stt_count = 0
        self.llm_count = 0
        self.tts_count = 0

    async def process_frame(self, frame, direction: FrameDirection):
        await super().process_frame(frame, direction)

        # Log STT transcriptions
        if isinstance(frame, TranscriptionFrame):
            self.stt_count += 1
            logger.warning(f"⭐ 🎙️  STT TRANSCRIPTION #{self.stt_count}: '{frame.text}' (confidence: {getattr(frame, 'confidence', 'N/A')})")
            logger.warning(f"⭐ USER SAID: '{frame.text}'")

        # Log LLM text responses
        elif isinstance(frame, TextFrame):
            self.llm_count += 1
            logger.warning(f"⭐ 🤖 LLM RESPONSE #{self.llm_count}: '{frame.text}'")
            logger.warning(f"⭐ AI RESPONDING: '{frame.text}'")

        # Log LLM completion
        elif isinstance(frame, LLMFullResponseEndFrame):
            logger.info(f"✅ LLM Response Complete")

        # Log TTS audio generation
        elif isinstance(frame, OutputAudioRawFrame):
            self.tts_count += 1
            if self.tts_count <= 5 or self.tts_count % 10 == 0:
                logger.info(f"🔊 TTS Audio Frame #{self.tts_count}: {len(frame.audio)} bytes")


class SimpleAudioOutput(FrameProcessor):
    """Simple audio output that serializes and sends via callback"""

    def __init__(self, send_callback: Callable[[dict], Awaitable[None]], serializer: RustPBXFrameSerializer):
        super().__init__()
        self.send_callback = send_callback
        self.serializer = serializer

    async def process_frame(self, frame, direction: FrameDirection):
        await super().process_frame(frame, direction)

        # Handle TTS audio output
        if isinstance(frame, OutputAudioRawFrame):
            # Serialize the frame to raw PCM bytes
            audio_data = await self.serializer.serialize(frame)

            if audio_data:
                # Send as binary audio via WebSocket
                await self.send_callback({
                    "type": "audio",
                    "audio_data": list(audio_data),  # Convert bytes to list for JSON
                    "sample_rate": SAMPLE_RATE,
                    "channels": CHANNELS,
                })
                logger.info(f"🔊 📤 Sent TTS audio: {len(audio_data)} bytes → WebSocket → RustPBX")


async def run_simple_bot(
    audio_queue: asyncio.Queue,
    send_callback: Callable[[dict], Awaitable[None]],
    connection_id: str
):
    """
    Run a simple Pipecat bot pipeline.

    Args:
        audio_queue: Queue receiving binary audio from RustPBX
        send_callback: Callback to send responses back to RustPBX
        connection_id: Unique connection identifier
    """
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
