// src/test_tts_ws.ts
import dotenv from 'dotenv';
import { WebSocketServer, WebSocket } from 'ws';
import {
  inference,
  tts as ttsCore,
  llm as llmCore,
  initializeLogger,
} from '@livekit/agents';

dotenv.config({ path: '.env.local' });

initializeLogger({ pretty: true, level: 'info' });

const PORT = Number(process.env.TTS_WS_PORT ?? 8090);

console.log('[INIT] Starting TTS WebSocket server');
console.log(`[INIT] WS Port: ${PORT}`);

// --- Shared LLM + TTS instances ---

const llm = new inference.LLM({
  model: 'openai/gpt-4.1-mini',
});

const tts = new inference.TTS({
  model: 'cartesia/sonic-3',
  voice: '9626c31c-bec5-4cca-baa8-f8ba9e84c8bc',
});

// Ask LLM something simple so you can hear a response
async function getLlmReply(): Promise<string> {
  const chatCtx = new llmCore.ChatContext();
  chatCtx.addMessage({
    role: 'system',
    content:
      'You are a helpful voice assistant. Reply in one or two short sentences.',
  });
  chatCtx.addMessage({
    role: 'user',
    content: 'Hello, can you please say something nice and friendly?',
  });

  console.log('[LLM] Starting chat() stream');
  const stream = await llm.chat({ chatCtx });

  let fullText = '';
  let chunkCount = 0;

  for await (const chunk of stream as any) {
    chunkCount++;
    console.log(`[LLM] Chunk #${chunkCount}:`, JSON.stringify(chunk, null, 2));

    if (chunk.delta && typeof chunk.delta.content === 'string') {
      fullText += chunk.delta.content;
    }
    if (chunk.output_text?.delta && typeof chunk.output_text.delta === 'string') {
      fullText += chunk.output_text.delta;
    }
  }

  fullText = fullText.trim();
  console.log('[LLM] Final text:', fullText || '<empty>');

  if (!fullText) {
    return 'Hello, I am your test voice assistant.';
  }
  return fullText;
}

// Stream TTS audio to a single WebSocket client
async function streamTtsToWs(ws: WebSocket, text: string) {
  console.log('[TTS] Streaming TTS for text:', text);

  const ttsStream = tts.stream();
  ttsStream.pushText(text);
  ttsStream.endInput();

  let sentConfig = false;

  for await (const ev of ttsStream) {
    if (ev === ttsCore.SynthesizeStream.END_OF_STREAM) {
      console.log('[TTS] END_OF_STREAM received');
      break;
    }

    const audioEvent = ev as ttsCore.SynthesizedAudio;
    const frame = audioEvent.frame;
    if (!frame) {
      console.log('[TTS] Event without frame, skipping');
      continue;
    }

    // Frame data is PCM Int16 (as Buffer)
    const pcm = frame.data as unknown as Buffer;

    if (!sentConfig) {
      // Send a small JSON config first so the browser knows sampleRate/channels
      const configPayload = {
        type: 'config',
        sampleRate: frame.sampleRate,
        channels: frame.channels,
      };
      console.log('[TTS] Sending config:', configPayload);
      ws.send(JSON.stringify(configPayload));
      sentConfig = true;
    }

    console.log(
      `[TTS] Sending audio chunk: ${pcm.length} bytes, ` +
        `sampleRate=${frame.sampleRate}, channels=${frame.channels}`,
    );

    // Send raw PCM as a binary message
    ws.send(pcm, { binary: true });
  }

  console.log('[TTS] Finished streaming TTS to WebSocket client');
}

async function handleClient(ws: WebSocket) {
  console.log('[WS] New client connected');

  ws.on('close', () => {
    console.log('[WS] Client disconnected');
  });

  ws.on('error', (err) => {
    console.error('[WS] Client error:', err);
  });

  try {
    // 1) Get a short LLM reply
    const replyText = await getLlmReply();

    // 2) Stream TTS of that reply to this ws
    await streamTtsToWs(ws, replyText);
  } catch (err) {
    console.error('[MAIN] Error handling client:', err);
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: 'error',
          message: 'Server error while generating TTS.',
        }),
      );
      ws.close();
    }
  } finally {
    if (ws.readyState === WebSocket.OPEN) {
      ws.close();
    }
  }
}

async function main() {
  console.log('[MAIN] Creating WebSocket server');
  const wss = new WebSocketServer({ port: PORT, host: '0.0.0.0' });

  wss.on('connection', (ws, req) => {
    console.log(
      `[MAIN] Client connected from ${req.socket.remoteAddress}:${req.socket.remotePort}`,
    );
    handleClient(ws).catch((err) =>
      console.error('[MAIN] Error in handleClient:', err),
    );
  });

  wss.on('error', (err) => {
    console.error('[MAIN] WebSocket server error:', err);
  });

  console.log(
    `[MAIN] TTS WS server listening on ws://localhost:${PORT} — open the HTML client to hear audio`,
  );
}

main().catch((err) => {
  console.error('[MAIN] Fatal error:', err);
  process.exit(1);
});
