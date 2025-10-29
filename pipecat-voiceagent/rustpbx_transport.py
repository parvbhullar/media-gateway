#!/usr/bin/env python3
"""
RustPBX WebSocket Transport for Pipecat

This transport integrates RustPBX WebRTC with Pipecat's pipeline architecture,
following the FastAPIWebsocketTransport pattern.

Based on: pipecat/transports/websocket/fastapi.py
"""

import asyncio
import time
from typing import Awaitable, Callable, Optional

from loguru import logger
from pydantic import BaseModel

from pipecat.frames.frames import (
    CancelFrame,
    EndFrame,
    Frame,
    InputAudioRawFrame,
    InterruptionFrame,
    OutputAudioRawFrame,
    StartFrame,
)
from pipecat.processors.frame_processor import FrameDirection
from pipecat.serializers.base_serializer import FrameSerializer, FrameSerializerType
from pipecat.transports.base_input import BaseInputTransport
from pipecat.transports.base_output import BaseOutputTransport
from pipecat.transports.base_transport import BaseTransport, TransportParams

try:
    from fastapi import WebSocket
    from starlette.websockets import WebSocketState
except ModuleNotFoundError as e:
    logger.error(f"Exception: {e}")
    logger.error("In order to use RustPBX transport, you need to `pip install fastapi websockets`.")
    raise Exception(f"Missing module: {e}")


class RustPBXTransportParams(TransportParams):
    """Configuration parameters for RustPBX WebSocket transport.

    Parameters:
        serializer: Frame serializer for encoding/decoding RustPBX audio frames.
        session_timeout: Session timeout in seconds, None for no timeout.
    """

    serializer: Optional[FrameSerializer] = None
    session_timeout: Optional[int] = None


class RustPBXTransportCallbacks(BaseModel):
    """Callback functions for RustPBX WebSocket events.

    Parameters:
        on_client_connected: Called when a client connects to the WebSocket.
        on_client_disconnected: Called when a client disconnects from the WebSocket.
    """

    on_client_connected: Callable[[WebSocket], Awaitable[None]]
    on_client_disconnected: Callable[[WebSocket], Awaitable[None]]


class RustPBXWebSocketClient:
    """WebSocket client wrapper for RustPBX connections.

    Manages the WebSocket connection lifecycle and message passing between
    RustPBX and Pipecat.
    """

    def __init__(
        self,
        websocket: WebSocket,
        is_binary: bool,
        callbacks: RustPBXTransportCallbacks
    ):
        """Initialize the RustPBX WebSocket client.

        Args:
            websocket: The FastAPI WebSocket connection.
            is_binary: Whether to use binary mode for messages.
            callbacks: Event callbacks for connection lifecycle.
        """
        self._websocket = websocket
        self._is_binary = is_binary
        self._callbacks = callbacks
        self._receive_task: Optional[asyncio.Task] = None
        self._connected = False

        logger.info(f"🔌 RustPBXWebSocketClient initialized (binary={is_binary})")

    async def setup(self):
        """Setup the WebSocket client and accept the connection."""
        try:
            # WebSocket should already be accepted by the endpoint
            self._connected = True
            await self._callbacks.on_client_connected(self._websocket)
            logger.info("✅ RustPBX WebSocket client connected")
        except Exception as e:
            logger.error(f"❌ Failed to setup RustPBX WebSocket client: {e}", exc_info=True)
            raise

    async def receive(self) -> Optional[bytes | str]:
        """Receive a message from the WebSocket.

        Returns:
            The received message (bytes or str), or None if disconnected.
        """
        if not self._connected:
            return None

        try:
            if self._is_binary:
                data = await self._websocket.receive_bytes()
            else:
                data = await self._websocket.receive_text()
            return data
        except Exception as e:
            logger.warning(f"⚠️  Error receiving from WebSocket: {e}")
            await self.disconnect()
            return None

    async def send(self, data: bytes | str):
        """Send a message to the WebSocket.

        Args:
            data: The message to send (bytes or str).
        """
        if not self._connected:
            logger.warning("⚠️  Cannot send: WebSocket not connected")
            return

        try:
            if self._is_binary:
                await self._websocket.send_bytes(data)
            else:
                await self._websocket.send_text(data)
        except Exception as e:
            logger.error(f"❌ Error sending to WebSocket: {e}", exc_info=True)
            await self.disconnect()

    async def disconnect(self):
        """Disconnect the WebSocket client."""
        if not self._connected:
            return

        self._connected = False

        try:
            await self._callbacks.on_client_disconnected(self._websocket)

            # Close the WebSocket if still open
            if self._websocket.client_state == WebSocketState.CONNECTED:
                await self._websocket.close()

            logger.info("🔌 RustPBX WebSocket client disconnected")
        except Exception as e:
            logger.error(f"❌ Error during disconnect: {e}", exc_info=True)

    @property
    def connected(self) -> bool:
        """Check if the client is connected."""
        return self._connected and self._websocket.client_state == WebSocketState.CONNECTED


class RustPBXInputTransport(BaseInputTransport):
    """Input transport for receiving audio from RustPBX via WebSocket.

    This processor receives raw audio frames from RustPBX WebRTC, deserializes them
    using the RustPBX serializer, and pushes them downstream to the STT service.
    """

    def __init__(
        self,
        transport: "RustPBXTransport",
        client: RustPBXWebSocketClient,
        params: RustPBXTransportParams,
        **kwargs
    ):
        """Initialize the RustPBX input transport.

        Args:
            transport: The parent transport instance.
            client: The WebSocket client.
            params: Transport configuration parameters.
        """
        super().__init__(params, **kwargs)
        self._transport = transport
        self._client = client
        self._params = params
        self._receive_task: Optional[asyncio.Task] = None

        logger.info("🎤 RustPBX input transport initialized")

    async def start(self, frame: StartFrame):
        """Start receiving audio from RustPBX.

        Args:
            frame: The StartFrame triggering the transport start.
        """
        await super().start(frame)

        # Start receiving messages from WebSocket
        self._receive_task = asyncio.create_task(self._receive_messages())
        logger.info("▶️  RustPBX input transport started")

    async def stop(self, frame: EndFrame):
        """Stop receiving audio from RustPBX.

        Args:
            frame: The EndFrame triggering the transport stop.
        """
        await super().stop(frame)

        # Cancel the receive task
        if self._receive_task:
            self._receive_task.cancel()
            try:
                await self._receive_task
            except asyncio.CancelledError:
                pass
            self._receive_task = None

        logger.info("⏹️  RustPBX input transport stopped")

    async def cancel(self, frame: CancelFrame):
        """Cancel the input transport.

        Args:
            frame: The CancelFrame triggering the cancellation.
        """
        await super().cancel(frame)
        await self.stop(EndFrame())

    async def _receive_messages(self):
        """Receive messages from the WebSocket and push frames downstream."""
        frame_count = 0

        logger.info("🚰 Started receiving messages from RustPBX WebSocket")

        while self._client.connected:
            try:
                # Receive data from WebSocket
                data = await self._client.receive()

                if data is None:
                    logger.info("🔌 WebSocket disconnected, stopping receive loop")
                    break

                # Deserialize the audio data to InputAudioRawFrame
                if self._params.serializer:
                    audio_frame = await self._params.serializer.deserialize(data)

                    if audio_frame:
                        frame_count += 1

                        # Set frame ID for turn tracking
                        audio_frame.id = frame_count

                        # Log first few frames
                        if frame_count <= 5:
                            logger.info(
                                f"🎤 Received frame #{frame_count}: {len(data)} bytes, "
                                f"type={type(audio_frame).__name__}"
                            )

                        # Push frame downstream to STT
                        await self.push_frame(audio_frame)

                        # Log periodically
                        if frame_count % 100 == 0:
                            logger.info(f"🎤 Received {frame_count} audio frames from RustPBX")
                    else:
                        logger.warning(f"⚠️  Failed to deserialize frame #{frame_count}")
                else:
                    logger.error("❌ No serializer configured for RustPBX transport")
                    break

            except asyncio.CancelledError:
                logger.info("🛑 Receive task cancelled")
                break
            except Exception as e:
                logger.error(f"❌ Error receiving message: {e}", exc_info=True)
                break

        logger.info(f"🏁 Stopped receiving messages (received {frame_count} frames total)")


class RustPBXOutputTransport(BaseOutputTransport):
    """Output transport for sending audio to RustPBX via WebSocket.

    This processor receives TTS audio frames from the pipeline, serializes them
    using the RustPBX serializer, and sends them back to RustPBX WebRTC.
    """

    def __init__(
        self,
        transport: "RustPBXTransport",
        client: RustPBXWebSocketClient,
        params: RustPBXTransportParams,
        **kwargs
    ):
        """Initialize the RustPBX output transport.

        Args:
            transport: The parent transport instance.
            client: The WebSocket client.
            params: Transport configuration parameters.
        """
        super().__init__(params, **kwargs)
        self._transport = transport
        self._client = client
        self._params = params
        self._audio_buffer = bytearray()

        logger.info("🔊 RustPBX output transport initialized")

    async def start(self, frame: StartFrame):
        """Start sending audio to RustPBX.

        Args:
            frame: The StartFrame triggering the transport start.
        """
        await super().start(frame)
        logger.info("▶️  RustPBX output transport started")

    async def stop(self, frame: EndFrame):
        """Stop sending audio to RustPBX.

        Args:
            frame: The EndFrame triggering the transport stop.
        """
        await super().stop(frame)

        # Send any remaining buffered audio
        if len(self._audio_buffer) > 0:
            await self._send_audio(bytes(self._audio_buffer))
            self._audio_buffer.clear()

        logger.info("⏹️  RustPBX output transport stopped")

    async def cancel(self, frame: CancelFrame):
        """Cancel the output transport.

        Args:
            frame: The CancelFrame triggering the cancellation.
        """
        await super().cancel(frame)
        await self.stop(EndFrame())

    async def process_frame(self, frame: Frame, direction: FrameDirection):
        """Process frames and send audio to RustPBX.

        Args:
            frame: The frame to process.
            direction: The direction the frame is traveling.
        """
        await super().process_frame(frame, direction)

        # Handle audio output frames from TTS
        if isinstance(frame, OutputAudioRawFrame):
            await self._handle_audio_frame(frame)

    async def _handle_audio_frame(self, frame: OutputAudioRawFrame):
        """Handle an output audio frame from TTS.

        Args:
            frame: The output audio frame to send to RustPBX.
        """
        # Serialize the frame to raw PCM bytes
        if self._params.serializer:
            audio_data = await self._params.serializer.serialize(frame)

            if audio_data:
                # Buffer the audio data
                self._audio_buffer.extend(audio_data)

                logger.debug(
                    f"🔊 Buffered TTS audio: {len(audio_data)} bytes "
                    f"(buffer now {len(self._audio_buffer)} bytes)"
                )

                # Send in chunks (e.g., 1 second of audio at 16kHz)
                chunk_size = 16000 * 2  # 1 second of 16-bit PCM at 16kHz

                while len(self._audio_buffer) >= chunk_size:
                    chunk = bytes(self._audio_buffer[:chunk_size])
                    self._audio_buffer = self._audio_buffer[chunk_size:]
                    await self._send_audio(chunk)
        else:
            logger.error("❌ No serializer configured for RustPBX transport")

    async def _send_audio(self, audio_data: bytes):
        """Send audio data to RustPBX via WebSocket.

        Args:
            audio_data: The raw PCM audio bytes to send.
        """
        try:
            logger.info(f"📤 Sending {len(audio_data)} bytes of audio to RustPBX")
            await self._client.send(audio_data)
        except Exception as e:
            logger.error(f"❌ Failed to send audio to RustPBX: {e}", exc_info=True)


class RustPBXTransport(BaseTransport):
    """RustPBX WebSocket transport for real-time audio streaming with Pipecat.

    Provides bidirectional WebSocket communication between RustPBX WebRTC and
    Pipecat's AI pipeline (STT → LLM → TTS).

    This transport follows the FastAPIWebsocketTransport pattern but is
    specifically designed for RustPBX's audio format and protocol.
    """

    def __init__(
        self,
        websocket: WebSocket,
        params: RustPBXTransportParams,
        input_name: Optional[str] = None,
        output_name: Optional[str] = None,
    ):
        """Initialize the RustPBX WebSocket transport.

        Args:
            websocket: The FastAPI WebSocket connection from RustPBX.
            params: Transport configuration parameters.
            input_name: Optional name for the input processor.
            output_name: Optional name for the output processor.
        """
        super().__init__(input_name=input_name, output_name=output_name)

        self._params = params

        # Setup callbacks
        self._callbacks = RustPBXTransportCallbacks(
            on_client_connected=self._on_client_connected,
            on_client_disconnected=self._on_client_disconnected,
        )

        # Determine if binary mode based on serializer
        is_binary = False
        if self._params.serializer:
            is_binary = self._params.serializer.type == FrameSerializerType.BINARY

        # Create WebSocket client
        self._client = RustPBXWebSocketClient(websocket, is_binary, self._callbacks)

        # Create input and output transports
        self._input = RustPBXInputTransport(
            self, self._client, self._params, name=self._input_name
        )
        self._output = RustPBXOutputTransport(
            self, self._client, self._params, name=self._output_name
        )

        # Register event handlers
        self._register_event_handler("on_client_connected")
        self._register_event_handler("on_client_disconnected")

        logger.info("🚀 RustPBX transport initialized")

    def input(self) -> RustPBXInputTransport:
        """Get the input transport processor.

        Returns:
            The RustPBX input transport instance.
        """
        return self._input

    def output(self) -> RustPBXOutputTransport:
        """Get the output transport processor.

        Returns:
            The RustPBX output transport instance.
        """
        return self._output

    async def _on_client_connected(self, websocket: WebSocket):
        """Handle client connected event.

        Args:
            websocket: The connected WebSocket.
        """
        await self._call_event_handler("on_client_connected", websocket)

    async def _on_client_disconnected(self, websocket: WebSocket):
        """Handle client disconnected event.

        Args:
            websocket: The disconnected WebSocket.
        """
        await self._call_event_handler("on_client_disconnected", websocket)
