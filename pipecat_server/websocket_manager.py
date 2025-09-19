"""
WebSocket connection management for Pipecat Media Server
"""

from fastapi import WebSocket
from loguru import logger
from typing import Dict, List, Optional
import asyncio
import json
from datetime import datetime, timezone


class WebSocketConnection:
    """Represents a WebSocket connection with metadata"""
    
    def __init__(self, websocket: WebSocket, client_id: str):
        self.websocket = websocket
        self.client_id = client_id
        self.connected_at = datetime.now(timezone.utc)
        self.last_ping = None
        self.is_alive = True
        
    async def send_message(self, message: dict) -> bool:
        """Send a message to this connection"""
        try:
            await self.websocket.send_text(json.dumps(message))
            return True
        except Exception as e:
            logger.error(f"Failed to send message to {self.client_id}: {e}")
            self.is_alive = False
            return False
            
    async def ping(self) -> bool:
        """Send a ping to test connection health"""
        try:
            ping_message = {
                "type": "ping",
                "timestamp": int(datetime.now(timezone.utc).timestamp() * 1000)
            }
            await self.websocket.send_text(json.dumps(ping_message))
            self.last_ping = datetime.now(timezone.utc)
            return True
        except Exception as e:
            logger.error(f"Failed to ping {self.client_id}: {e}")
            self.is_alive = False
            return False
            
    async def close(self):
        """Close the WebSocket connection"""
        try:
            await self.websocket.close()
        except Exception as e:
            logger.debug(f"Error closing connection for {self.client_id}: {e}")
        finally:
            self.is_alive = False


class WebSocketManager:
    """Manages WebSocket connections and broadcasting"""
    
    def __init__(self):
        self.connections: Dict[str, WebSocketConnection] = {}
        self.ping_interval = 30  # seconds
        self.ping_task: Optional[asyncio.Task] = None
        
    async def connect(self, websocket: WebSocket, client_id: str):
        """Accept and register a new WebSocket connection"""
        await websocket.accept()
        
        # If client already connected, close old connection
        if client_id in self.connections:
            await self.disconnect_client(client_id)
            
        connection = WebSocketConnection(websocket, client_id)
        self.connections[client_id] = connection
        
        logger.info(f"🔌 WebSocket connected: {client_id} (total: {len(self.connections)})")
        
        # Start ping task if this is the first connection
        if len(self.connections) == 1 and not self.ping_task:
            self.ping_task = asyncio.create_task(self._ping_loop())
            
    async def disconnect(self, websocket: WebSocket):
        """Disconnect a WebSocket by websocket instance"""
        client_id = None
        for cid, conn in self.connections.items():
            if conn.websocket == websocket:
                client_id = cid
                break
                
        if client_id:
            await self.disconnect_client(client_id)
            
    async def disconnect_client(self, client_id: str):
        """Disconnect a specific client"""
        if client_id in self.connections:
            connection = self.connections[client_id]
            await connection.close()
            del self.connections[client_id]
            logger.info(f"🔌 WebSocket disconnected: {client_id} (remaining: {len(self.connections)})")
            
            # Stop ping task if no connections remain
            if len(self.connections) == 0 and self.ping_task:
                self.ping_task.cancel()
                self.ping_task = None
                
    async def disconnect_all(self):
        """Disconnect all WebSocket connections"""
        logger.info("🔌 Disconnecting all WebSocket connections")
        
        # Cancel ping task
        if self.ping_task:
            self.ping_task.cancel()
            self.ping_task = None
            
        # Close all connections
        for client_id in list(self.connections.keys()):
            await self.disconnect_client(client_id)
            
    async def send_to_client(self, client_id: str, message: dict) -> bool:
        """Send a message to a specific client"""
        if client_id in self.connections:
            return await self.connections[client_id].send_message(message)
        return False
        
    async def broadcast(self, message: dict, exclude: Optional[List[str]] = None):
        """Broadcast a message to all connected clients"""
        exclude = exclude or []
        sent_count = 0
        failed_clients = []
        
        for client_id, connection in self.connections.items():
            if client_id not in exclude:
                if await connection.send_message(message):
                    sent_count += 1
                else:
                    failed_clients.append(client_id)
                    
        # Clean up failed connections
        for client_id in failed_clients:
            await self.disconnect_client(client_id)
            
        if sent_count > 0:
            logger.debug(f"📡 Broadcast message to {sent_count} clients")
            
    async def broadcast_message(self, message: dict, exclude: Optional[List[str]] = None):
        """Convenience method for broadcasting messages"""
        await self.broadcast(message, exclude)
            
    def connection_count(self) -> int:
        """Get the number of active connections"""
        return len(self.connections)
        
    def get_connection_info(self) -> List[dict]:
        """Get information about all connections"""
        return [
            {
                "client_id": conn.client_id,
                "connected_at": conn.connected_at.isoformat(),
                "last_ping": conn.last_ping.isoformat() if conn.last_ping else None,
                "is_alive": conn.is_alive
            }
            for conn in self.connections.values()
        ]
        
    async def _ping_loop(self):
        """Background task to ping all connections periodically"""
        while True:
            try:
                await asyncio.sleep(self.ping_interval)
                
                if not self.connections:
                    break
                    
                logger.debug(f"🏓 Pinging {len(self.connections)} connections")
                failed_clients = []
                
                for client_id, connection in self.connections.items():
                    if not await connection.ping():
                        failed_clients.append(client_id)
                        
                # Clean up failed connections
                for client_id in failed_clients:
                    await self.disconnect_client(client_id)
                    
            except asyncio.CancelledError:
                logger.debug("Ping loop cancelled")
                break
            except Exception as e:
                logger.error(f"Error in ping loop: {e}")
                
        logger.debug("Ping loop stopped")