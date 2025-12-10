import dotenv from 'dotenv';
import { WebSocketServer, WebSocket } from 'ws';
import * as fs from 'fs';
import * as path from 'path';

import {
    inference,
    stt as sttCore,
    tts as ttsCore,
    llm as llmCore,
    initializeLogger
} from '@livekit/agents';
import { AudioFrame } from '@livekit/rtc-node';
import { fileURLToPath } from "url";

dotenv.config({ path: '.env.local' });

initializeLogger({ pretty: true, level: 'info' });

const PORT = Number(process.env.PORT ?? 8765);
const SAMPLE_RATE = 16_000;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

console.log('[INIT] Starting WAV File Test Server');
console.log(`[INIT] Port: ${PORT}`);
console.log(`[INIT] Sample Rate: ${SAMPLE_RATE}`);

type Role = 'system' | 'user' | 'assistant';

interface ChatMessage {
    role: Role;
    content: string;
}

// Shared LLM + TTS for all calls
console.log('[INIT] Creating LLM instance');
const llm = new inference.LLM({
    model: 'openai/gpt-4.1-mini',
});
console.log('[INIT] LLM created successfully');

console.log('[INIT] Creating TTS instance');
const tts = new inference.TTS({
    model: 'cartesia/sonic-3',
    voice: '9626c31c-bec5-4cca-baa8-f8ba9e84c8bc',
});
console.log('[INIT] TTS created successfully');

// WAV file header parsing
function parseWavHeader(buffer: Buffer) {
    // Basic WAV header validation and parsing
    const riffHeader = buffer.toString('ascii', 0, 4);
    if (riffHeader !== 'RIFF') {
        throw new Error('Not a valid WAV file - missing RIFF header');
    }

    const waveHeader = buffer.toString('ascii', 8, 12);
    if (waveHeader !== 'WAVE') {
        throw new Error('Not a valid WAV file - missing WAVE header');
    }

    // Find fmt chunk
    let offset = 12;
    while (offset < buffer.length - 8) {
        const chunkId = buffer.toString('ascii', offset, offset + 4);
        const chunkSize = buffer.readUInt32LE(offset + 4);

        if (chunkId === 'fmt ') {
            const audioFormat = buffer.readUInt16LE(offset + 8);
            const numChannels = buffer.readUInt16LE(offset + 10);
            const sampleRate = buffer.readUInt32LE(offset + 12);
            const bitsPerSample = buffer.readUInt16LE(offset + 22);

            console.log('[WAV] WAV file info:', {
                audioFormat,
                numChannels,
                sampleRate,
                bitsPerSample
            });

            return {
                audioFormat,
                numChannels,
                sampleRate,
                bitsPerSample,
                dataOffset: offset + 8 + chunkSize
            };
        }

        offset += 8 + chunkSize;
    }

    throw new Error('WAV file missing fmt chunk');
}

function findDataChunk(buffer: Buffer, startOffset: number) {
    let offset = startOffset;
    while (offset < buffer.length - 8) {
        const chunkId = buffer.toString('ascii', offset, offset + 4);
        const chunkSize = buffer.readUInt32LE(offset + 4);

        if (chunkId === 'data') {
            console.log(`[WAV] Found data chunk at offset ${offset}, size: ${chunkSize} bytes`);
            return {
                dataStart: offset + 8,
                dataSize: chunkSize
            };
        }

        offset += 8 + chunkSize;
    }

    throw new Error('WAV file missing data chunk');
}

class WavFileTestSession {
    private stt: inference.STT<'assemblyai/universal-streaming'>;
    private sttStream: sttCore.SpeechStream | null = null;
    private messages: ChatMessage[] = [];
    private closed = false;
    private sessionId: string;
    private audioFrameCount = 0;

    constructor() {
        this.sessionId = Math.random().toString(36).substring(7);
        console.log(`[SESSION-${this.sessionId}] Creating new WavFileTestSession`);

        console.log(`[SESSION-${this.sessionId}] Creating STT instance`);
        this.stt = new inference.STT({
            model: 'assemblyai/universal-streaming',
            language: 'en',
        });
        console.log(`[SESSION-${this.sessionId}] STT created successfully`);

        this.messages.push({
            role: 'system',
            content:
                'You are a helpful voice AI assistant. Be concise, friendly, and avoid emojis.',
        });
        console.log(`[SESSION-${this.sessionId}] System message added to chat history`);
    }

    private encodeWavFromPcm(
        pcmData: Buffer,
        sampleRate: number,
        numChannels: number,
        bitsPerSample: number
    ): Buffer {
        const byteRate = sampleRate * numChannels * bitsPerSample / 8;
        const blockAlign = numChannels * bitsPerSample / 8;
        const dataSize = pcmData.length;
    
        const wavBuffer = Buffer.alloc(44 + dataSize);
        let offset = 0;
    
        // RIFF header
        wavBuffer.write('RIFF', offset); offset += 4;
        wavBuffer.writeUInt32LE(36 + dataSize, offset); offset += 4;
        wavBuffer.write('WAVE', offset); offset += 4;
    
        // fmt  chunk
        wavBuffer.write('fmt ', offset); offset += 4;
        wavBuffer.writeUInt32LE(16, offset); offset += 4;          // Subchunk1Size (16 for PCM)
        wavBuffer.writeUInt16LE(1, offset); offset += 2;           // AudioFormat (1 = PCM)
        wavBuffer.writeUInt16LE(numChannels, offset); offset += 2;
        wavBuffer.writeUInt32LE(sampleRate, offset); offset += 4;
        wavBuffer.writeUInt32LE(byteRate, offset); offset += 4;
        wavBuffer.writeUInt16LE(blockAlign, offset); offset += 2;
        wavBuffer.writeUInt16LE(bitsPerSample, offset); offset += 2;
    
        // data chunk
        wavBuffer.write('data', offset); offset += 4;
        wavBuffer.writeUInt32LE(dataSize, offset); offset += 4;
    
        // PCM data
        pcmData.copy(wavBuffer, offset);
    
        return wavBuffer;
    }
    

    async start() {
        console.log(`[SESSION-${this.sessionId}] Starting WavFileTestSession`);

        console.log(`[SESSION-${this.sessionId}] Testing LLM connectivity`);
        const testResult = await this.testLlmDirectly();
        console.log(`[SESSION-${this.sessionId}] LLM test result: "${testResult}"`);

        console.log(`[SESSION-${this.sessionId}] Creating STT stream`);
        try {
            this.sttStream = this.stt.stream();
            console.log(`[SESSION-${this.sessionId}] STT stream created successfully`);
        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error creating STT stream:`, error);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Starting STT event handler`);
        this.handleSttEvents().catch((err) => {
            console.error(`[SESSION-${this.sessionId}] STT loop error:`, err);
            this.close();
        });
        console.log(`[SESSION-${this.sessionId}] STT event handler started`);

        // Process WAV file
        await this.processWavFile();
    }

    private async processWavFile() {
        console.log(`[SESSION-${this.sessionId}] Processing WAV file from sample_audio folder`);

        const sampleAudioDir = path.join(__dirname, '..', 'sample_audio');
        console.log(`[SESSION-${this.sessionId}] Looking for WAV files in: ${sampleAudioDir}`);

        try {
            const files = fs.readdirSync(sampleAudioDir);
            const wavFiles = files.filter(file => file.toLowerCase().endsWith('.wav'));

            if (wavFiles.length === 0) {
                console.error(`[SESSION-${this.sessionId}] No WAV files found in ${sampleAudioDir}`);
                return;
            }

            console.log(`[SESSION-${this.sessionId}] Found WAV files:`, wavFiles);

            // Use the first WAV file
            const wavFile = wavFiles[0];
            if (!wavFile) {
                console.error(`[SESSION-${this.sessionId}] No valid WAV file found`);
                return;
            }
            const wavPath = path.join(sampleAudioDir, wavFile);

            console.log(`[SESSION-${this.sessionId}] Processing WAV file: ${wavPath}`);

            const wavBuffer = fs.readFileSync(wavPath);
            console.log(`[SESSION-${this.sessionId}] Read ${wavBuffer.length} bytes from WAV file`);

            // Parse WAV header
            const wavInfo = parseWavHeader(wavBuffer);
            console.log(`[SESSION-${this.sessionId}] WAV info:`, wavInfo);

            // Find data chunk
            const dataChunk = findDataChunk(wavBuffer, wavInfo.dataOffset);
            console.log(`[SESSION-${this.sessionId}] Data chunk info:`, dataChunk);

            // Extract audio data
            const audioData = wavBuffer.subarray(dataChunk.dataStart, dataChunk.dataStart + dataChunk.dataSize);
            console.log(`[SESSION-${this.sessionId}] Extracted ${audioData.length} bytes of audio data`);

            // Process audio in chunks (simulate streaming)
            await this.streamAudioData(audioData, wavInfo);

        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error processing WAV file:`, error);
        }
    }

    private async streamAudioData(audioData: Buffer, wavInfo: any) {
        console.log(`[SESSION-${this.sessionId}] Starting to stream audio data`);

        const chunkSizeMs = 20; // 20ms chunks
        const chunkSizeBytes = Math.floor((SAMPLE_RATE * chunkSizeMs / 1000) * (wavInfo.bitsPerSample / 8) * wavInfo.numChannels);

        console.log(`[SESSION-${this.sessionId}] Chunk size: ${chunkSizeBytes} bytes (${chunkSizeMs}ms)`);

        let offset = 0;
        let chunkCount = 0;

        while (offset < audioData.length && !this.closed) {
            const remainingBytes = audioData.length - offset;
            const currentChunkSize = Math.min(chunkSizeBytes, remainingBytes);

            const chunk = audioData.subarray(offset, offset + currentChunkSize);
            chunkCount++;

            console.log(`[SESSION-${this.sessionId}] Processing chunk ${chunkCount}, offset: ${offset}, size: ${currentChunkSize} bytes`);

            await this.handleAudioChunk(chunk, wavInfo);

            offset += currentChunkSize;

            // Simulate real-time streaming with delay
            await new Promise(resolve => setTimeout(resolve, chunkSizeMs));
        }

        console.log(`[SESSION-${this.sessionId}] Finished streaming ${chunkCount} audio chunks`);

        // End the STT stream
        setTimeout(() => {
            if (this.sttStream) {
                console.log(`[SESSION-${this.sessionId}] Ending STT stream`);
                this.sttStream.endInput();
            }
        }, 1000);
    }

    private async handleAudioChunk(chunk: Buffer, wavInfo: any) {
        this.audioFrameCount++;

        if (this.audioFrameCount % 50 === 0) {
            console.log(`[SESSION-${this.sessionId}] Processing audio chunk ${this.audioFrameCount}, size: ${chunk.length} bytes`);
        }

        if (!this.sttStream) {
            console.error(`[SESSION-${this.sessionId}] No STT stream available`);
            return;
        }

        try {
            let samples: Int16Array;

            if (wavInfo.bitsPerSample === 16) {
                // 16-bit PCM
                samples = new Int16Array(chunk.buffer, chunk.byteOffset, chunk.byteLength / 2);
            } else if (wavInfo.bitsPerSample === 8) {
                // 8-bit PCM - convert to 16-bit
                samples = new Int16Array(chunk.length);
                for (let i = 0; i < chunk.length; i++) {
                    samples[i] = (chunk[i]! - 128) * 256; // Convert unsigned 8-bit to signed 16-bit
                }
            } else {
                console.error(`[SESSION-${this.sessionId}] Unsupported bit depth: ${wavInfo.bitsPerSample}`);
                return;
            }

            // Handle stereo to mono conversion if needed
            if (wavInfo.numChannels === 2) {
                const monoSamples = new Int16Array(samples.length / 2);
                for (let i = 0; i < monoSamples.length; i++) {
                    // Average left and right channels
                    monoSamples[i] = Math.floor((samples[i * 2]! + samples[i * 2 + 1]!) / 2);
                }
                samples = monoSamples;
            }

            // Resample if needed (basic resampling)
            if (wavInfo.sampleRate !== SAMPLE_RATE) {
                samples = this.resample(samples, wavInfo.sampleRate, SAMPLE_RATE);
            }

            // Create AudioFrame
            const audioFrame = new AudioFrame(
                samples,
                SAMPLE_RATE,
                1, // mono
                samples.length
            );

            if (this.audioFrameCount % 50 === 0) {
                console.log(`[SESSION-${this.sessionId}] AudioFrame created:`, {
                    sampleRate: audioFrame.sampleRate,
                    numChannels: audioFrame.channels,
                    samplesPerChannel: audioFrame.samplesPerChannel,
                    duration: (audioFrame.samplesPerChannel / audioFrame.sampleRate * 1000).toFixed(2) + 'ms'
                });
            }

            this.sttStream.pushFrame(audioFrame);

            if (this.audioFrameCount % 50 === 0) {
                console.log(`[SESSION-${this.sessionId}] AudioFrame pushed to STT (total frames: ${this.audioFrameCount})`);
            }

        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error processing audio chunk:`, error);
        }
    }

    private resample(input: Int16Array, inputRate: number, outputRate: number): Int16Array {
        if (inputRate === outputRate) {
            return input;
        }

        const ratio = inputRate / outputRate;
        const outputLength = Math.floor(input.length / ratio);
        const output = new Int16Array(outputLength);

        for (let i = 0; i < outputLength; i++) {
            const srcIndex = i * ratio;
            const index = Math.floor(srcIndex);
            if (index < input.length) {
                output[i] = input[index] ?? 0;
            }
        }

        console.log(`[SESSION-${this.sessionId}] Resampled from ${inputRate}Hz to ${outputRate}Hz: ${input.length} -> ${output.length} samples`);
        return output;
    }

    private async handleSttEvents() {
        console.log(`[SESSION-${this.sessionId}] handleSttEvents started`);

        if (!this.sttStream) {
            console.error(`[SESSION-${this.sessionId}] No STT stream in handleSttEvents`);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Starting STT event loop`);

        try {
            let eventCount = 0;
            for await (const ev of this.sttStream) {
                eventCount++;
                console.log(`[SESSION-${this.sessionId}] *** STT EVENT #${eventCount} RECEIVED ***`);
                console.log(`[SESSION-${this.sessionId}] STT event type:`, ev.type);
                console.log(`[SESSION-${this.sessionId}] Full STT event:`, JSON.stringify(ev, null, 2));

                if (ev.type === sttCore.SpeechEventType.FINAL_TRANSCRIPT) {
                    console.log(`[SESSION-${this.sessionId}] Final transcript event received`);
                    const text = ev.alternatives?.[0]?.text?.trim();
                    console.log(`[SESSION-${this.sessionId}] Extracted text: "${text}"`);

                    if (!text) {
                        console.log(`[SESSION-${this.sessionId}] No text content, skipping`);
                        continue;
                    }

                    console.log(`[SESSION-${this.sessionId}] User said: "${text}"`);
                    this.messages.push({ role: 'user', content: text });
                    console.log(`[SESSION-${this.sessionId}] Added user message to chat history`);

                    console.log(`[SESSION-${this.sessionId}] Starting response generation`);
                    await this.respondToUser();
                } else if (ev.type === sttCore.SpeechEventType.INTERIM_TRANSCRIPT) {
                    console.log(`[SESSION-${this.sessionId}] Interim transcript:`, ev.alternatives?.[0]?.text);
                } else {
                    console.log(`[SESSION-${this.sessionId}] Other STT event type: ${ev.type}`);
                }
            }
        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error in STT event loop:`, error);
        }

        console.log(`[SESSION-${this.sessionId}] STT event loop ended`);
    }

    private async respondToUser() {
        console.log(`[SESSION-${this.sessionId}] respondToUser called`);

        if (this.closed) {
            console.log(`[SESSION-${this.sessionId}] Session closed, not responding`);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Calling LLM for response`);
        const replyText = await this.callLlm();
        console.log(`[SESSION-${this.sessionId}] LLM response: "${replyText}"`);

        this.messages.push({ role: 'assistant', content: replyText });
        console.log(`[SESSION-${this.sessionId}] Added assistant message to chat history`);

        console.log(`[SESSION-${this.sessionId}] Starting TTS streaming`);
        await this.streamTts(replyText);
        console.log(`[SESSION-${this.sessionId}] TTS streaming completed`);
    }

    // private async callLlm(): Promise<string> {
    //     console.log(`[SESSION-${this.sessionId}] callLlm started`);
        
    //     const chatCtx = new llmCore.ChatContext();
    //     console.log(`[SESSION-${this.sessionId}] Created chat context`);

    //     for (const m of this.messages) {
    //         chatCtx.addMessage({
    //             role: m.role as llmCore.ChatRole,
    //             content: m.content,
    //         });
    //     }
    //     console.log(`[SESSION-${this.sessionId}] Added ${this.messages.length} messages to context`);
    //     console.log(`[SESSION-${this.sessionId}] Starting LLM chat stream`);
    //     const stream = await llm.chat({ chatCtx });
    //     console.log(`[SESSION-${this.sessionId}] LLM stream created successfully`);

    //     let fullText = '';
    //     let chunkCount = 0;

    //     try {
    //         console.log(`[SESSION-${this.sessionId}] Processing LLM stream chunks`);
            
    //         for await (const chunk of stream as any) {
    //             chunkCount++;
    //             console.log(`[SESSION-${this.sessionId}] LLM chunk #${chunkCount} received:`, JSON.stringify(chunk, null, 2));
                
    //             // Try different chunk structures based on the OpenAI API format
    //             if (chunk.choices && chunk.choices.length > 0) {
    //                 const choice = chunk.choices[0];
                    
    //                 // Check for delta content (streaming format)
    //                 if (choice.delta && choice.delta.content) {
    //                     const deltaContent = choice.delta.content;
    //                     if (typeof deltaContent === 'string') {
    //                         fullText += deltaContent;
    //                         console.log(`[SESSION-${this.sessionId}] Added delta content: "${deltaContent}"`);
    //                     }
    //                 }
                    
    //                 // Check for direct message content (non-streaming format)
    //                 if (choice.message && choice.message.content) {
    //                     const messageContent = choice.message.content;
    //                     if (typeof messageContent === 'string') {
    //                         fullText += messageContent;
    //                         console.log(`[SESSION-${this.sessionId}] Added message content: "${messageContent}"`);
    //                     }
    //                 }
                    
    //                 // Check for text field directly
    //                 if (choice.text && typeof choice.text === 'string') {
    //                     fullText += choice.text;
    //                     console.log(`[SESSION-${this.sessionId}] Added text content: "${choice.text}"`);
    //                 }
    //             }
                
    //             // Alternative structure - sometimes content is at root level
    //             if (chunk.content && typeof chunk.content === 'string') {
    //                 fullText += chunk.content;
    //                 console.log(`[SESSION-${this.sessionId}] Added root content: "${chunk.content}"`);
    //             }
                
    //             // Another alternative - text at root level
    //             if (chunk.text && typeof chunk.text === 'string') {
    //                 fullText += chunk.text;
    //                 console.log(`[SESSION-${this.sessionId}] Added root text: "${chunk.text}"`);
    //             }
                
    //             console.log(`[SESSION-${this.sessionId}] Current accumulated text length: ${fullText.length}`);
    //         }
            
    //         console.log(`[SESSION-${this.sessionId}] LLM streaming completed after ${chunkCount} chunks`);
            
    //     } catch (error) {
    //         console.error(`[SESSION-${this.sessionId}] Error processing LLM stream:`, error);
    //         if (error instanceof Error) {
    //             console.error(`[SESSION-${this.sessionId}] Error stack:`, error.stack);
    //         } else {
    //             console.error(`[SESSION-${this.sessionId}] Unknown error type:`, error);
    //         }
    //     }

    //     fullText = fullText.trim();
    //     console.log(`[SESSION-${this.sessionId}] Final accumulated LLM text: "${fullText}"`);

    //     if (!fullText) {
    //         console.log(`[SESSION-${this.sessionId}] No content from LLM, using fallback`);
    //         console.error(`[SESSION-${this.sessionId}] Debug: Stream processed ${chunkCount} chunks but got no text content`);
            
    //         // Try a simple non-streaming call as fallback
    //         try {
    //             console.log(`[SESSION-${this.sessionId}] Attempting non-streaming LLM call as fallback`);
    //             const simpleResponse = await llm.chat({ 
    //                 chatCtx,
    //                 // Try without streaming
    //             });
    //             console.log(`[SESSION-${this.sessionId}] Simple response:`, simpleResponse);
                
    //             // If it's not a stream, it might return the response directly
    //             if (typeof simpleResponse === 'string') {
    //                 return simpleResponse;
    //             }
                
    //             // Check if it has a text property
    //             if (simpleResponse && 'text' in simpleResponse && typeof simpleResponse.text === 'string') {
    //                 return simpleResponse.text;
    //             }
                
    //             // Check if it has choices
    //             if (simpleResponse && typeof simpleResponse === 'object' && 'choices' in simpleResponse && Array.isArray((simpleResponse as any).choices) && (simpleResponse as any).choices[0]) {
    //                 const choice = (simpleResponse as { choices: any[] }).choices[0];
    //                 if (choice.message && choice.message.content) {
    //                     return choice.message.content;
    //                 }
    //                 if (choice.text) {
    //                     return choice.text;
    //                 }
    //             }
                
    //         } catch (fallbackError) {
    //             console.error(`[SESSION-${this.sessionId}] Fallback LLM call also failed:`, fallbackError);
    //         }
            
    //         return 'Sorry, I had a problem generating a response.';
    //     }

    //     console.log(`[SESSION-${this.sessionId}] LLM generated ${fullText.length} characters`);
    //     return fullText;
    // }

    // ...existing code...

    // Add this method for testing LLM directly
    
    private async callLlm(): Promise<string> {
        console.log(`[SESSION-${this.sessionId}] callLlm started`);
        
        const chatCtx = new llmCore.ChatContext();
        console.log(`[SESSION-${this.sessionId}] Created chat context`);
    
        for (const m of this.messages) {
            chatCtx.addMessage({
                role: m.role as llmCore.ChatRole,
                content: m.content,
            });
        }
        console.log(`[SESSION-${this.sessionId}] Added ${this.messages.length} messages to context`);
    
        console.log(`[SESSION-${this.sessionId}] Starting LLM chat stream`);
        const stream = await llm.chat({ chatCtx });
        console.log(`[SESSION-${this.sessionId}] LLM stream created successfully`);
    
        let fullText = '';
        let chunkCount = 0;
    
        try {
            console.log(`[SESSION-${this.sessionId}] Processing LLM stream chunks`);
    
            for await (const chunk of stream as any) {
                chunkCount++;
                console.log(
                    `[SESSION-${this.sessionId}] LLM chunk #${chunkCount} received:`,
                    JSON.stringify(chunk, null, 2)
                );
    
                // LiveKit LLM stream format: { id, delta?: { content?: string, ... }, usage? }
                if (chunk.delta && typeof chunk.delta.content === 'string') {
                    fullText += chunk.delta.content;
                    console.log(
                        `[SESSION-${this.sessionId}] Added delta content: "${chunk.delta.content}"`
                    );
                }
    
                // (Optional) some providers might send `output_text` style events; keep this as a backup:
                if (chunk.output_text?.delta && typeof chunk.output_text.delta === 'string') {
                    fullText += chunk.output_text.delta;
                    console.log(
                        `[SESSION-${this.sessionId}] Added output_text.delta: "${chunk.output_text.delta}"`
                    );
                }
    
                console.log(
                    `[SESSION-${this.sessionId}] Current accumulated text length: ${fullText.length}`
                );
            }
    
            console.log(
                `[SESSION-${this.sessionId}] LLM streaming completed after ${chunkCount} chunks`
            );
        } catch (error) {
            console.error(
                `[SESSION-${this.sessionId}] Error processing LLM stream:`,
                error
            );
        }
    
        fullText = fullText.trim();
        console.log(
            `[SESSION-${this.sessionId}] Final accumulated LLM text: "${fullText}"`
        );
    
        if (!fullText) {
            console.log(
                `[SESSION-${this.sessionId}] No content from LLM, using fallback`
            );
            return 'Sorry, I had a problem generating a response.';
        }
    
        console.log(
            `[SESSION-${this.sessionId}] LLM generated ${fullText.length} characters`
        );
        return fullText;
    }
    
    
    private async testLlmDirectly() {
        console.log(`[SESSION-${this.sessionId}] Testing LLM directly`);
        
        try {
            const testChatCtx = new llmCore.ChatContext();
            testChatCtx.addMessage({
                role: 'user' as llmCore.ChatRole,
                content: 'Hello, can you respond with just "Hello back"?',
            });
            
            console.log(`[SESSION-${this.sessionId}] Calling LLM with simple test message`);
            const stream = await llm.chat({ chatCtx: testChatCtx });
            
            let response = '';
            for await (const chunk of stream as any) {
                console.log(`[SESSION-${this.sessionId}] Test LLM chunk:`, JSON.stringify(chunk, null, 2));
                
                if (chunk.choices?.[0]?.delta?.content) {
                    response += chunk.choices[0].delta.content;
                }
            }
            
            console.log(`[SESSION-${this.sessionId}] Test LLM response: "${response}"`);
            return response.trim() || 'LLM test failed';
            
        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] LLM test error:`, error);
            return 'LLM test error';
        }
    }

    private async streamTts(text: string) {
        console.log(`[SESSION-${this.sessionId}] streamTts started with text: "${text}"`);
    
        if (this.closed) {
            console.log(`[SESSION-${this.sessionId}] Session closed, not streaming TTS`);
            return;
        }
    
        console.log(`[SESSION-${this.sessionId}] Creating TTS stream`);
        const ttsStream = tts.stream();
        console.log(`[SESSION-${this.sessionId}] TTS stream created`);
    
        console.log(`[SESSION-${this.sessionId}] Pushing text to TTS`);
        ttsStream.pushText(text);
        ttsStream.endInput();
        console.log(`[SESSION-${this.sessionId}] Text pushed and input ended`);
    
        const pcmChunks: Buffer[] = [];
    
        console.log(`[SESSION-${this.sessionId}] Processing TTS stream`);
        for await (const ev of ttsStream) {
            console.log(`[SESSION-${this.sessionId}] TTS event received`);
    
            if (ev === ttsCore.SynthesizeStream.END_OF_STREAM) {
                console.log(`[SESSION-${this.sessionId}] TTS end of stream`);
                break;
            }
    
            const audioEvent = ev as ttsCore.SynthesizedAudio;
            const frame = audioEvent.frame;
            if (!frame) {
                console.log(`[SESSION-${this.sessionId}] TTS event has no frame`);
                continue;
            }
    
            // frame.data is typically Int16Array or Buffer
            let buf: Buffer;
            if (Buffer.isBuffer(frame.data)) {
                buf = frame.data;
            } else {
                const int16 = frame.data as Int16Array;
                buf = Buffer.from(int16.buffer, int16.byteOffset, int16.byteLength);
            }
    
            console.log(
                `[SESSION-${this.sessionId}] TTS audio frame: ${buf.length} bytes`
            );
            pcmChunks.push(buf);
        }
    
        if (pcmChunks.length === 0) {
            console.warn(`[SESSION-${this.sessionId}] No TTS PCM data collected`);
            return;
        }
    
        const pcmData = Buffer.concat(pcmChunks);
        console.log(
            `[SESSION-${this.sessionId}] Total TTS PCM size: ${pcmData.length} bytes`
        );
    
        // Encode to WAV (16kHz, mono, 16-bit)
        const wavBuffer = this.encodeWavFromPcm(
            pcmData,
            SAMPLE_RATE,
            1,
            16
        );
    
        const outputDir = path.join(__dirname, '..', 'sample_audio');
        const outputFile = path.join(
            outputDir,
            `response_${this.sessionId}_${Date.now()}.wav`
        );
    
        fs.writeFileSync(outputFile, wavBuffer);
        console.log(
            `[SESSION-${this.sessionId}] Wrote TTS WAV file: ${outputFile}`
        );
        console.log(
            `[SESSION-${this.sessionId}] You can now open this file in any audio player to hear the LLM response`
        );
    
        console.log(`[SESSION-${this.sessionId}] TTS streaming finished`);
    }
    

    close() {
        console.log(`[SESSION-${this.sessionId}] close() called`);

        if (this.closed) {
            console.log(`[SESSION-${this.sessionId}] Already closed`);
            return;
        }
        this.closed = true;

        console.log(`[SESSION-${this.sessionId}] Closing WavFileTestSession...`);

        if (this.sttStream) {
            console.log(`[SESSION-${this.sessionId}] Ending STT stream input`);
            this.sttStream.endInput();
            this.sttStream = null;
            console.log(`[SESSION-${this.sessionId}] STT stream cleaned up`);
        }

        console.log(`[SESSION-${this.sessionId}] WavFileTestSession closed successfully`);
    }
}

async function main() {

    console.log('[MAIN] Starting WAV file test');

    // Create sample_audio directory if it doesn't exist
    const sampleAudioDir = path.join(__dirname, '..', 'sample_audio');
    if (!fs.existsSync(sampleAudioDir)) {
        console.log('[MAIN] Creating sample_audio directory');
        fs.mkdirSync(sampleAudioDir, { recursive: true });
        console.log(`[MAIN] Created directory: ${sampleAudioDir}`);
        console.log(`[MAIN] Please place your WAV files in: ${sampleAudioDir}`);
        return;
    }

    const exampleFilePath = path.join(sampleAudioDir, 'example.wav');
    if (!fs.existsSync(exampleFilePath)) {
        console.error(`[MAIN] The file "example.wav" does not exist in ${sampleAudioDir}`);
        console.error(`[MAIN] Please ensure the file is placed in the correct directory.`);
        return;
    }

    console.log(`[MAIN] Sample audio directory: ${sampleAudioDir}`);

    // Start the test session
    const session = new WavFileTestSession();
    await session.start();

    // Keep the process alive
    setTimeout(() => {
        console.log('[MAIN] Test completed, closing session');
        session.close();
    }, 30000); // Run for 30 seconds max
}

console.log('[INIT] Starting WAV file test application');
main().catch((err) => {
    console.error('[MAIN] Fatal error:', err);
    process.exit(1);
});
console.log('[INIT] Application started');
