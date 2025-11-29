import {
  type JobContext,
  type JobProcess,
  WorkerOptions,
  cli,
  defineAgent,
  inference,
  metrics,
  voice,
} from '@livekit/agents';
import * as livekit from '@livekit/agents-plugin-livekit';
import { LocalAudioTrack } from '@livekit/rtc-node';
import { AudioSource } from '@livekit/rtc-node';
import * as silero from '@livekit/agents-plugin-silero';
import { BackgroundVoiceCancellation } from '@livekit/noise-cancellation-node';
import dotenv from 'dotenv';
import { fileURLToPath } from 'node:url';
import { WebSocketServer, WebSocket } from 'ws';
import { TrackPublishOptions } from '@livekit/rtc-node'; // Adjusted to the base package
// Removed invalid import as '@livekit/rtc-node/dist/proto/audio_frame' does not exist


dotenv.config({ path: '.env.local' });

// ========== WebSocket server for Rust audio ==========
const WS_PORT = Number(process.env.WS_PORT ?? 8081);

const wss = new WebSocketServer({ port: WS_PORT });

// Store audio source reference globally so WebSocket can access it
let globalAudioSource: AudioSource | null = null;
let isProcessing = false;

wss.on('connection', (socket: WebSocket) => {
  console.log('[WS] Rust audio client connected from localhost:8080');

  socket.on('message', (data, isBinary) => {
    if (isBinary) {
      const audioBuffer = data as Buffer;
      
      // Process audio directly through the agent's audio source
      if (globalAudioSource && isProcessing) {
        try {
          // Convert buffer to Int16Array (assuming 16-bit PCM audio)
          const samples = new Int16Array(
            audioBuffer.buffer, 
            audioBuffer.byteOffset, 
            audioBuffer.byteLength / 2
          );
          
          // Push audio directly into the agent's pipeline
          globalAudioSource.captureFrame({
            data: samples,
            sampleRate: 16000,
            channels: 1,
            samplesPerChannel: samples.length,
          } as any);
          
          // Optional: log periodically instead of every frame
          // if (Math.random() < 0.01) {
          //   console.log(`[WS] processed ${audioBuffer.byteLength} bytes`);
          // }
        } catch (error) {
          console.error('[WS] Error processing audio frame:', error);
        }
      } else {
        console.warn('[WS] Audio source not ready, dropping audio packet');
      }
    } else {
      console.log('[WS] text message from Rust:', data.toString());
    }
  });

  socket.on('close', () => {
    console.log('[WS] Rust audio client disconnected');
    isProcessing = false;
  });
  
  socket.on('error', (err: any) => {
    console.error('[WS] error:', err);
  });
});

console.log(`[WS] listening on ws://0.0.0.0:${WS_PORT} for Rust server audio`);
// ========== END WS server ==========

class Assistant extends voice.Agent {
  constructor() {
    super({
      instructions: `You are a helpful voice AI assistant. The user is interacting with you via voice through a PBX system.
      You eagerly assist users with their questions by providing information from your extensive knowledge.
      Your responses are concise, to the point, and without any complex formatting or punctuation including emojis, asterisks, or other symbols.
      You are curious, friendly, and have a sense of humor.
      Remember that this is a phone conversation, so keep your responses natural and conversational.`,
    });
  }
}

export default defineAgent({
  prewarm: async (proc: JobProcess) => {
    proc.userData.vad = await silero.VAD.load();
  },
  entry: async (ctx: JobContext) => {
    // Set up a voice AI pipeline
    const session = new voice.AgentSession({
      stt: new inference.STT({
        model: 'assemblyai/universal-streaming',
        language: 'en',
      }),

      llm: new inference.LLM({
        model: 'openai/gpt-4.1-mini',
      }),

      tts: new inference.TTS({
        model: 'cartesia/sonic-3',
        voice: '9626c31c-bec5-4cca-baa8-f8ba9e84c8bc',
      }),

      turnDetection: new livekit.turnDetector.MultilingualModel(),
      vad: ctx.proc.userData.vad! as silero.VAD,
    });

    // Metrics collection
    const usageCollector = new metrics.UsageCollector();
    session.on(voice.AgentSessionEventTypes.MetricsCollected, (ev) => {
      metrics.logMetrics(ev.metrics);
      usageCollector.collect(ev.metrics);
    });

    const logUsage = async () => {
      const summary = usageCollector.getSummary();
      console.log(`Usage: ${JSON.stringify(summary)}`);
    };

    ctx.addShutdownCallback(logUsage);

    // Start the session
    await session.start({
      agent: new Assistant(),
      room: ctx.room,
      inputOptions: {
        noiseCancellation: BackgroundVoiceCancellation(),
      },
    });

    // Join the room
    await ctx.connect();

    // Create audio source for Rust audio input
    // Adjust sample rate based on your Rust server's audio format
    // Common formats: 8000 (telephony), 16000 (wideband), 48000 (high quality)
    const audioSource = new AudioSource(16000, 1); // 16kHz mono
    globalAudioSource = audioSource; // Make it accessible to WebSocket handler
    
    const audioTrack = LocalAudioTrack.createAudioTrack('rust-pbx-audio', audioSource);

    (audioTrack as any).name = "rust-pbx-audio";

    const opts = new TrackPublishOptions({
      // valid fields are:
      // source?: TrackSource
      // disableDtx?: boolean
      // disableRed?: boolean
      // forceRelay?: boolean
      // simulcast?: boolean
      // videoCodec?: string
      // videoEncoding?: VideoEncoding
    });
    
    if (ctx.room.localParticipant) {
      await ctx.room.localParticipant.publishTrack(audioTrack as any, opts);
    } else {
      console.error('[Agent] localParticipant is undefined, cannot publish track');
    }
    
    console.log('[Agent] Audio track published, ready to receive from Rust PBX');
    isProcessing = true;

    // Handle cleanup
    ctx.addShutdownCallback(async () => {
      isProcessing = false;
      globalAudioSource = null;
      console.log('[Agent] Shutting down, stopped processing Rust audio');
    });
  },
});

cli.runApp(new WorkerOptions({ agent: fileURLToPath(import.meta.url) }));