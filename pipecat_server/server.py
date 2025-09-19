"""
FastAPI server application with WebSocket support for RustPBX integration
"""

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse, FileResponse
from fastapi.staticfiles import StaticFiles
from loguru import logger
import asyncio
import json
import time
import uuid
from typing import Dict, List, Optional
from datetime import datetime, timezone

from config import Settings
from models import Room, RoomCreate, RoomUpdate, HealthResponse
from ai_services import AIProcessor
from websocket_manager import WebSocketManager


class PipecatServer:
    """Main Pipecat server application"""
    
    def __init__(self, settings: Settings):
        self.settings = settings
        self.rooms: Dict[str, Room] = {}
        self.websocket_manager = WebSocketManager()
        self.ai_processor = AIProcessor(settings)
        self.rustpbx_connected = False
        self.active_sessions = 0
        self.audio_streaming_active = False
        
    async def startup(self):
        """Initialize server components"""
        logger.info("🔧 Initializing Pipecat server components")
        await self.ai_processor.initialize()
        logger.info("✅ Server initialization complete")
        
    async def shutdown(self):
        """Clean up server resources"""
        logger.info("🧹 Cleaning up server resources")
        await self.websocket_manager.disconnect_all()
        await self.ai_processor.cleanup()
        logger.info("✅ Server cleanup complete")
        
    def create_room(self, room_data: RoomCreate) -> Room:
        """Create a new processing room"""
        room_id = str(uuid.uuid4())
        room = Room(
            id=room_id,
            name=room_data.name,
            system_prompt=room_data.system_prompt,
            created_at=datetime.now(timezone.utc),
            active=True
        )
        self.rooms[room_id] = room
        logger.info(f"📱 Created room: {room_id} ({room.name})")
        return room
        
    def get_room(self, room_id: str) -> Optional[Room]:
        """Get room by ID"""
        return self.rooms.get(room_id)
        
    def list_rooms(self) -> List[Room]:
        """List all active rooms"""
        return list(self.rooms.values())
        
    def delete_room(self, room_id: str) -> bool:
        """Delete a room"""
        if room_id in self.rooms:
            del self.rooms[room_id]
            logger.info(f"🗑️ Deleted room: {room_id}")
            return True
        return False
        
    def update_room_prompt(self, room_id: str, prompt: str) -> bool:
        """Update room system prompt"""
        if room_id in self.rooms:
            self.rooms[room_id].system_prompt = prompt
            logger.info(f"📝 Updated prompt for room: {room_id}")
            return True
        return False
        
    async def handle_dashboard_message(self, message: dict) -> Optional[dict]:
        """Handle WebSocket messages from dashboard"""
        msg_type = message.get("type")
        
        if msg_type == "ping":
            return {"type": "pong", "timestamp": int(time.time() * 1000)}
            
        elif msg_type == "status_request":
            return {
                "type": "status",
                "server_status": {
                    "version": "1.0.0",
                    "uptime": datetime.now(timezone.utc).isoformat(),
                    "active_rooms": len(self.rooms),
                    "active_connections": self.websocket_manager.connection_count()
                },
                "ai_services": {
                    "stt": {"status": "online" if self.ai_processor.stt.client else "offline"},
                    "llm": {"status": "online" if self.ai_processor.llm.client else "offline"},
                    "tts": {"status": "online" if self.ai_processor.tts.client else "offline"}
                }
            }
            
        elif msg_type == "stats_request":
            stats = self.ai_processor.get_stats()
            return {
                "type": "stats",
                "stats": {
                    "active_connections": self.websocket_manager.connection_count(),
                    "messages_processed": stats.get("messages_processed", 0),
                    "audio_frames_processed": stats.get("audio_frames_processed", 0),
                    "errors": stats.get("errors", 0)
                }
            }
            
        elif msg_type == "test_pipeline":
            # Test AI pipeline with text message
            test_text = message.get("text", "Hello, this is a test.")
            try:
                response = await self.ai_processor.llm.generate_response(test_text, "dashboard_test")
                return {
                    "type": "test_result",
                    "success": True,
                    "input": test_text,
                    "output": response,
                    "timestamp": datetime.now(timezone.utc).isoformat()
                }
            except Exception as e:
                return {
                    "type": "test_result",
                    "success": False,
                    "error": str(e),
                    "timestamp": datetime.now(timezone.utc).isoformat()
                }
                
        return None
        
    async def handle_rustpbx_configure(self, message: dict) -> dict:
        """Handle configuration message from RustPBX"""
        try:
            call_id = message.get("call_id", message.get("session_id", "default"))
            config = message.get("config", {})
            
            # Extract Pipecat configuration
            pipecat_config = config.get("pipecat", {})
            if pipecat_config:
                logger.info(f"🔧 Configuring Pipecat for call {call_id}")
                logger.info(f"   System prompt: {pipecat_config.get('systemPrompt', 'default')}")
                logger.info(f"   Use for AI: {pipecat_config.get('useForAI', False)}")
            
            # Store configuration for this call
            # This could be expanded to store per-call settings
            
            return {
                "type": "configured",
                "call_id": call_id,
                "status": "success",
                "timestamp": int(time.time() * 1000)
            }
            
        except Exception as e:
            logger.error(f"❌ Configuration error: {e}")
            return {
                "type": "error",
                "message": str(e),
                "timestamp": int(time.time() * 1000)
            }

    async def notify_dashboard_status_update(self):
        """Notify dashboard of status changes"""
        status_message = {
            "type": "rustpbx_status_update",
            "rustpbx_connected": self.rustpbx_connected,
            "audio_streaming_active": self.audio_streaming_active,
            "active_sessions": self.active_sessions,
            "timestamp": int(time.time() * 1000)
        }
        
        # Send to all dashboard connections
        await self.websocket_manager.send_to_client("dashboard", status_message)


def create_app(settings: Settings) -> FastAPI:
    """Create and configure the FastAPI application"""
    
    app = FastAPI(
        title="Pipecat Media Server",
        description="AI-powered media processing server for RustPBX integration",
        version="1.0.0",
        docs_url="/docs",
        redoc_url="/redoc"
    )
    
    # CORS middleware
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )
    
    # Initialize server instance
    server = PipecatServer(settings)
    app.state.server = server
    
    # Mount static files
    app.mount("/static", StaticFiles(directory="static"), name="static")
    
    @app.on_event("startup")
    async def startup_event():
        await server.startup()
    
    @app.on_event("shutdown")
    async def shutdown_event():
        await server.shutdown()
    
    # Root route - serve dashboard
    @app.get("/")
    async def root():
        """Serve the dashboard HTML page"""
        return FileResponse("static/index.html")
    
    # Health check endpoint
    @app.get("/health", response_model=HealthResponse)
    async def health_check():
        """Health check endpoint"""
        return HealthResponse(
            status="healthy",
            timestamp=datetime.now(timezone.utc),
            version="1.0.0",
            active_rooms=len(server.rooms),
            active_connections=server.websocket_manager.connection_count()
        )
    
    # Room management endpoints
    @app.post("/rooms", response_model=Room)
    async def create_room(room_data: RoomCreate):
        """Create a new processing room"""
        return server.create_room(room_data)
    
    @app.get("/rooms", response_model=List[Room])
    async def list_rooms():
        """List all active rooms"""
        return server.list_rooms()
    
    @app.get("/rooms/{room_id}", response_model=Room)
    async def get_room(room_id: str):
        """Get room details"""
        room = server.get_room(room_id)
        if not room:
            raise HTTPException(status_code=404, detail="Room not found")
        return room
    
    @app.delete("/rooms/{room_id}")
    async def delete_room(room_id: str):
        """Delete a room"""
        if not server.delete_room(room_id):
            raise HTTPException(status_code=404, detail="Room not found")
        return {"message": "Room deleted successfully"}
    
    @app.post("/rooms/{room_id}/prompt")
    async def update_room_prompt(room_id: str, prompt_data: RoomUpdate):
        """Update room system prompt"""
        if not server.update_room_prompt(room_id, prompt_data.system_prompt):
            raise HTTPException(status_code=404, detail="Room not found")
        return {"message": "Prompt updated successfully"}
    
    # WebSocket endpoint for RustPBX integration
    @app.websocket("/ws/rustpbx")
    async def rustpbx_websocket(websocket: WebSocket):
        """WebSocket endpoint for RustPBX communication"""
        await server.websocket_manager.connect(websocket, "rustpbx")
        logger.info("🔌 RustPBX WebSocket connected")
        
        # Update connection status
        server.rustpbx_connected = True
        await server.notify_dashboard_status_update()
        
        # Send initial handshake
        await websocket.send_text(json.dumps({
            "type": "connected",
            "server": "pipecat-media-server",
            "version": "1.0.0",
            "timestamp": int(time.time() * 1000)
        }))
        
        try:
            while True:
                # Receive message from RustPBX
                data = await websocket.receive_text()
                message = json.loads(data)
                
                message_type = message.get('type', message.get('event', message.get('command', 'unknown')))
                logger.info(f"📨 RustPBX → Pipecat: {message_type}")
                logger.debug(f"Full message: {message}")
                
                # Handle RustPBX-specific message types
                response = None
                
                if message_type == "configure":
                    # Handle configuration from RustPBX call setup
                    response = await server.handle_rustpbx_configure(message)
                elif message_type == "audio_frame" or "samples" in message or "data" in message:
                    # Handle audio data for AI processing
                    server.audio_streaming_active = True
                    if server.active_sessions == 0:
                        server.active_sessions = 1
                        await server.notify_dashboard_status_update()
                    response = await server.ai_processor.process_message(message, server.websocket_manager)
                elif message_type == "ping":
                    response = {"type": "pong", "timestamp": int(time.time() * 1000)}
                else:
                    # Try to process with AI processor
                    response = await server.ai_processor.process_message(message, server.websocket_manager)
                
                if response:
                    # Send response back to RustPBX
                    await websocket.send_text(json.dumps(response))
                    response_type = response.get('event', response.get('type', 'unknown'))
                    logger.info(f"📤 Pipecat → RustPBX: {response_type}")
                
        except WebSocketDisconnect:
            logger.info("🔌 RustPBX WebSocket disconnected")
        except Exception as e:
            logger.error(f"❌ RustPBX WebSocket error: {e}")
        finally:
            # Update connection status
            server.rustpbx_connected = False
            server.audio_streaming_active = False
            server.active_sessions = 0
            await server.notify_dashboard_status_update()
            await server.websocket_manager.disconnect(websocket)
    
    # WebSocket endpoint for dashboard
    @app.websocket("/ws/dashboard")
    async def dashboard_websocket(websocket: WebSocket):
        """WebSocket endpoint for dashboard monitoring"""
        await server.websocket_manager.connect(websocket, "dashboard")
        logger.info("🔌 Dashboard WebSocket connected")
        
        try:
            # Send initial status
            await websocket.send_text(json.dumps({
                "type": "status",
                "server_status": {
                    "version": "1.0.0",
                    "uptime": datetime.now(timezone.utc).isoformat()
                },
                "ai_services": {
                    "stt": {"status": "online" if server.ai_processor.stt.client else "offline"},
                    "llm": {"status": "online" if server.ai_processor.llm.client else "offline"},
                    "tts": {"status": "online" if server.ai_processor.tts.client else "offline"}
                }
            }))
            
            while True:
                data = await websocket.receive_text()
                message = json.loads(data)
                
                # Handle dashboard requests
                response = await server.handle_dashboard_message(message)
                if response:
                    await websocket.send_text(json.dumps(response))
                
        except WebSocketDisconnect:
            logger.info("🔌 Dashboard WebSocket disconnected")
        except Exception as e:
            logger.error(f"❌ Dashboard WebSocket error: {e}")
        finally:
            await server.websocket_manager.disconnect(websocket)

    # WebSocket endpoint for general clients
    @app.websocket("/ws/{client_id}")
    async def general_websocket(websocket: WebSocket, client_id: str):
        """General WebSocket endpoint for other clients"""
        await server.websocket_manager.connect(websocket, client_id)
        logger.info(f"🔌 Client {client_id} WebSocket connected")
        
        try:
            while True:
                data = await websocket.receive_text()
                message = json.loads(data)
                
                # Echo message for now (can be extended for specific client logic)
                await websocket.send_text(json.dumps({
                    "type": "echo",
                    "original": message,
                    "timestamp": datetime.now(timezone.utc).isoformat()
                }))
                
        except WebSocketDisconnect:
            logger.info(f"🔌 Client {client_id} WebSocket disconnected")
        except Exception as e:
            logger.error(f"❌ WebSocket error for {client_id}: {e}")
        finally:
            await server.websocket_manager.disconnect(websocket)
    
    return app