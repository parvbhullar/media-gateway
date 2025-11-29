import WebSocket, { WebSocketServer } from 'ws';

const wss = new WebSocketServer({ port: 8050 });

console.log("🎧 WebSocket Audio Server is running on ws://localhost:8050");

wss.on('connection', (ws) => {
  console.log("🔗 Client connected");

  ws.on('message', (data) => {
    const byteLength = Buffer.isBuffer(data)
      ? data.byteLength
      : Array.isArray(data)
      ? data.reduce((sum, buf) => sum + buf.byteLength, 0)
      : data instanceof ArrayBuffer
      ? data.byteLength
      : 0;
    console.log(`📩 Received audio chunk (${byteLength} bytes)`);

    // Respond back that bytes are received
    ws.send("Audio bytes received!");
  });

  ws.on('close', () => {
    console.log("❌ Client disconnected");
  });
});
