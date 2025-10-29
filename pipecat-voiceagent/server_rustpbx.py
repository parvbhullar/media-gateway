#!/usr/bin/env python3
"""
Pipecat Voice Agent Server for RustPBX Integration

This server replaces the original SmallWebRTC-based server with a WebSocket server
that accepts connections from RustPBX and processes audio through the Pipecat pipeline.

Audio Flow:
    RustPBX WebRTC -> WebSocket -> Pipecat Pipeline (STT -> LLM -> TTS) -> WebSocket -> RustPBX
"""

import asyncio
import json
import os
import sys
import time
from typing import Dict, Optional

import websockets
from dotenv import load_dotenv
from loguru import logger
from websockets.server import WebSocketServerProtocol

from bot_simple import run_simple_bot

load_dotenv(override=True)

# Server configuration
HOST = os.getenv("SERVER_HOST", "0.0.0.0")
PORT = int(os.getenv("SERVER_PORT", "8765"))
WS_PATH = "/ws/rustpbx"

# Active connections
active_connections: Dict[str, "RustPBXConnection"] = {}


class RustPBXConnection:
    """Manages a single RustPBX WebSocket connection with Pipecat processing"""

    def __init__(self, websocket: WebSocketServerProtocol, connection_id: str):
        self.websocket = websocket
        self.connection_id = connection_id
        self.bot_task: Optional[asyncio.Task] = None
        self.audio_queue: asyncio.Queue = asyncio.Queue()
        self.is_running = False
        self.audio_frame_count = 0

    async def start(self):
        """Start the bot pipeline for this connection"""
        self.is_running = True
        logger.info(f"Starting bot pipeline for connection {self.connection_id}")

        # Create and start the bot pipeline
        try:
            self.bot_task = asyncio.create_task(
                run_simple_bot(
                    self.audio_queue,
                    self.send_response,
                    self.connection_id
                )
            )

            # Send connection confirmation
            await self.send_response({
                "type": "connected",
                "server": "pipecat-voiceagent",
                "version": "1.0.0",
                "timestamp": int(time.time() * 1000)
            })

        except Exception as e:
            logger.error(f"Failed to start bot pipeline: {e}")
            await self.send_error(f"Failed to start bot pipeline: {e}")
            raise

    async def handle_audio(self, audio_data: bytes):
        """Handle incoming audio data from RustPBX"""
        try:
            self.audio_frame_count += 1

            # Log first few frames and then periodically
            if self.audio_frame_count <= 5:
                logger.info(f"📥 Received audio frame #{self.audio_frame_count} ({len(audio_data)} bytes) - queuing for pipeline")
            elif self.audio_frame_count % 100 == 0:
                logger.info(f"📥 Received audio frame #{self.audio_frame_count} ({len(audio_data)} bytes)")

            # Queue audio for processing by the bot pipeline
            await self.audio_queue.put(audio_data)

            # Log queue size for first few frames
            if self.audio_frame_count <= 5:
                logger.info(f"📊 Audio queue size: {self.audio_queue.qsize()} items")

        except Exception as e:
            logger.error(f"❌ Error handling audio: {e}")
            await self.send_error(f"Audio processing error: {e}")

    async def handle_message(self, message: str):
        """Handle incoming JSON messages from RustPBX"""
        try:
            data = json.loads(message)
            command = data.get("command")

            if command == "configure":
                await self.handle_configure(data)
            elif command == "ping":
                await self.send_response({
                    "type": "pong",
                    "timestamp": data.get("timestamp", int(time.time() * 1000))
                })
            elif command == "disconnect":
                logger.info(f"Received disconnect command: {data.get('reason', 'No reason')}")
                await self.stop()
            else:
                logger.warning(f"Unknown command: {command}")

        except json.JSONDecodeError as e:
            logger.error(f"Failed to parse JSON message: {e}")
            await self.send_error(f"Invalid JSON: {e}")
        except Exception as e:
            logger.error(f"Error handling message: {e}")
            await self.send_error(f"Message handling error: {e}")

    async def handle_configure(self, config: dict):
        """Handle configuration message from RustPBX"""
        logger.info(f"Received configuration: {config}")

        room_id = config.get("room_id", self.connection_id)
        system_prompt = config.get("system_prompt")

        # Send configuration confirmation
        await self.send_response({
            "type": "configured",
            "call_id": room_id,
            "status": "ready",
            "timestamp": int(time.time() * 1000)
        })

        # TODO: Update bot pipeline with new configuration if needed
        # This could involve updating the LLM system prompt
        if system_prompt:
            logger.info(f"System prompt updated: {system_prompt}")

    async def send_response(self, response: dict):
        """Send a JSON response to RustPBX"""
        try:
            # Check if websocket is still open (websockets 13.x uses state)
            if hasattr(self.websocket, 'state'):
                # websockets 13.x
                from websockets.protocol import State
                if self.websocket.state != State.OPEN:
                    logger.warning("WebSocket closed, cannot send response")
                    return
            elif hasattr(self.websocket, 'open'):
                # older websockets
                if not self.websocket.open:
                    logger.warning("WebSocket closed, cannot send response")
                    return

            message = json.dumps(response)
            await self.websocket.send(message)
        except Exception as e:
            logger.error(f"Failed to send response: {e}")

    async def send_error(self, error_message: str, code: Optional[int] = None):
        """Send an error response to RustPBX"""
        await self.send_response({
            "type": "error",
            "message": error_message,
            "code": code,
            "timestamp": int(time.time() * 1000)
        })

    async def stop(self):
        """Stop the bot pipeline and clean up"""
        logger.info(f"Stopping connection {self.connection_id}")
        self.is_running = False

        # Cancel bot task
        if self.bot_task and not self.bot_task.done():
            self.bot_task.cancel()
            try:
                await self.bot_task
            except asyncio.CancelledError:
                pass

        # Close WebSocket if still open
        try:
            # Check connection state (websockets 13.x compatible)
            if hasattr(self.websocket, 'state'):
                from websockets.protocol import State
                if self.websocket.state == State.OPEN:
                    await self.websocket.close()
            elif hasattr(self.websocket, 'open') and self.websocket.open:
                await self.websocket.close()
        except Exception as e:
            logger.debug(f"Error closing websocket: {e}")


async def handle_client(websocket: WebSocketServerProtocol):
    """Handle a new WebSocket client connection"""

    # Get path from websocket request (websockets 13.x style)
    path = websocket.request.path

    # Validate path
    if path != WS_PATH:
        logger.warning(f"Invalid WebSocket path: {path}")
        await websocket.close(1008, f"Invalid path. Expected: {WS_PATH}")
        return

    # Generate connection ID
    connection_id = f"rustpbx_{int(time.time() * 1000)}"
    logger.info(f"New RustPBX connection: {connection_id} from {websocket.remote_address}")

    # Create connection handler
    connection = RustPBXConnection(websocket, connection_id)
    active_connections[connection_id] = connection

    try:
        # Start the bot pipeline
        await connection.start()

        # Message loop
        async for message in websocket:
            if isinstance(message, bytes):
                # Binary message = audio data
                await connection.handle_audio(message)
            elif isinstance(message, str):
                # Text message = JSON command
                await connection.handle_message(message)
            else:
                logger.warning(f"Unknown message type: {type(message)}")

    except websockets.exceptions.ConnectionClosed as e:
        logger.info(f"Connection {connection_id} closed: {e.code} - {e.reason}")
    except Exception as e:
        logger.error(f"Error in connection {connection_id}: {e}", exc_info=True)
    finally:
        # Clean up
        await connection.stop()
        active_connections.pop(connection_id, None)
        logger.info(f"Connection {connection_id} cleaned up. Active connections: {len(active_connections)}")


def process_request(connection, request):
    """
    Custom request processor to handle RustPBX's non-standard Connection header.

    RustPBX's tokio-tungstenite may send 'Connection: keep-alive' instead of
    'Connection: Upgrade', which is technically valid for HTTP/1.1 but not
    expected by websockets library.

    Args:
        connection: WebSocket connection object
        request: Request object with headers attribute
    """
    # Check if this is a WebSocket upgrade request by looking for Upgrade header
    upgrade = request.headers.get("Upgrade")
    if upgrade and upgrade.lower() == "websocket":
        # This is a valid WebSocket request, accept it regardless of Connection header
        logger.debug(f"Accepting WebSocket connection from {connection.remote_address}")
        return None  # None means accept the connection

    # Not a WebSocket upgrade, reject
    logger.warning(f"Rejecting non-WebSocket request from {connection.remote_address}")
    return (400, [], b"WebSocket upgrade required\n")


async def main():
    """Start the WebSocket server"""
    logger.info(f"Starting Pipecat Voice Agent Server for RustPBX")
    logger.info(f"Listening on ws://{HOST}:{PORT}{WS_PATH}")
    logger.info(f"Press Ctrl+C to stop the server")
    logger.info(f"")
    logger.info(f"Note: This server accepts WebSocket connections from RustPBX")
    logger.info(f"      which may use non-standard 'Connection: keep-alive' header")

    # Start WebSocket server with custom request processor
    async with websockets.serve(
        handle_client,
        HOST,
        PORT,
        ping_interval=20,
        ping_timeout=10,
        max_size=10 * 1024 * 1024,  # 10MB max message size for audio
        process_request=process_request,  # Custom processor to handle RustPBX headers
    ):
        # Run forever
        await asyncio.Future()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        logger.info("Server stopped by user")
        sys.exit(0)
    except Exception as e:
        logger.error(f"Server error: {e}", exc_info=True)
        sys.exit(1)
