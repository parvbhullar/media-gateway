#!/usr/bin/env python3
"""
FastAPI-based Pipecat Voice Agent Server for RustPBX Integration

This server uses FastAPI WebSocket transport pattern (similar to the Exotel example)
to integrate RustPBX WebRTC with Pipecat's AI pipeline.

Audio Flow:
    RustPBX WebRTC -> FastAPI WebSocket -> RustPBX Transport -> Pipecat Pipeline -> RustPBX Transport -> FastAPI WebSocket -> RustPBX

Based on: https://github.com/pipecat-ai/pipecat-examples/blob/main/exotel-chatbot/inbound/bot.py
"""

import os
from typing import Optional

import uvicorn
from dotenv import load_dotenv
from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from loguru import logger

from bot_rustpbx import run_bot_with_transport

load_dotenv(override=True)

# Server configuration
HOST = os.getenv("SERVER_HOST", "0.0.0.0")
PORT = int(os.getenv("SERVER_PORT", "8765"))

# Create FastAPI app
app = FastAPI(
    title="Pipecat Voice Agent for RustPBX",
    description="AI-powered voice agent using Pipecat with RustPBX WebRTC",
    version="1.0.0"
)


@app.get("/")
async def root():
    """Health check endpoint"""
    return {
        "service": "pipecat-voiceagent",
        "status": "running",
        "version": "1.0.0",
        "endpoints": {
            "websocket": "/ws/rustpbx",
            "health": "/"
        }
    }


@app.get("/health")
async def health():
    """Health check endpoint for monitoring"""
    return {"status": "healthy"}


@app.websocket("/ws/rustpbx")
async def websocket_endpoint(websocket: WebSocket):
    """
    WebSocket endpoint for RustPBX connections.

    This endpoint accepts WebSocket connections from RustPBX and creates
    a Pipecat pipeline with RustPBX transport for audio processing.
    """
    connection_id = f"rustpbx_{id(websocket)}"

    logger.info(f"🔌 New WebSocket connection: {connection_id}")

    try:
        # Accept the WebSocket connection
        await websocket.accept()
        logger.info(f"✅ WebSocket accepted: {connection_id}")

        # Run the bot pipeline with transport
        await run_bot_with_transport(websocket, connection_id)

    except WebSocketDisconnect as e:
        logger.info(f"👋 WebSocket disconnected: {connection_id} (code={e.code})")
    except Exception as e:
        logger.error(f"❌ Error in WebSocket connection {connection_id}: {e}", exc_info=True)
        try:
            await websocket.close(code=1011, reason=str(e)[:123])  # Max 123 bytes for close reason
        except Exception:
            pass
    finally:
        logger.info(f"🧹 Cleaning up connection: {connection_id}")


def main():
    """Start the FastAPI server"""
    logger.info("🚀 Starting Pipecat Voice Agent Server (FastAPI)")
    logger.info(f"📍 Listening on http://{HOST}:{PORT}")
    logger.info(f"🔌 WebSocket endpoint: ws://{HOST}:{PORT}/ws/rustpbx")
    logger.info("Press Ctrl+C to stop")

    uvicorn.run(
        app,
        host=HOST,
        port=PORT,
        log_level="info",
        access_log=True,
    )


if __name__ == "__main__":
    main()
