#!/usr/bin/env python3
"""
Simple startup script for Pipecat Media Server
"""

import asyncio
import sys
import os

# Add current directory to path
sys.path.insert(0, os.getcwd())

try:
    import asyncio
    import uvicorn
    from loguru import logger
    from config import get_settings
    from server import create_app
    
    async def main():
        """Main entry point for the Pipecat media server"""
        settings = get_settings()
        
        logger.info("🚀 Starting Pipecat Media Server v1.0.0")
        logger.info(f"📡 Server will bind to {settings.host}:{settings.port}")
        logger.info(f"🔧 Log level: {settings.log_level}")
        
        # Create FastAPI application
        app = create_app(settings)
        
        # Configure uvicorn
        config = uvicorn.Config(
            app,
            host=settings.host,
            port=settings.port,
            log_level=settings.log_level.lower(),
            access_log=True,
            ws_ping_interval=30,
            ws_ping_timeout=10,
        )
        
        server = uvicorn.Server(config)
        
        try:
            logger.info("✅ Pipecat Media Server started successfully")
            await server.serve()
        except KeyboardInterrupt:
            logger.info("🛑 Server shutdown requested")
        except Exception as e:
            logger.error(f"❌ Server error: {e}")
            raise
        finally:
            logger.info("🔌 Pipecat Media Server stopped")
    
    if __name__ == "__main__":
        print("🚀 Starting Pipecat Media Server...")
        asyncio.run(main())
        
except KeyboardInterrupt:
    print("\n🛑 Server stopped by user")
except ImportError as e:
    print(f"❌ Import error: {e}")
    print("💡 Make sure you're in the pipecat_server directory and virtual environment is activated")
    sys.exit(1)
except Exception as e:
    print(f"❌ Server error: {e}")
    sys.exit(1)