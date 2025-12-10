/**
 * WebSocket connection management for Pipecat Dashboard
 */

class WebSocketManager {
    constructor() {
        this.ws = null;
        this.reconnectAttempts = 0;
        this.maxReconnectAttempts = 5;
        this.reconnectDelay = 1000;
        this.isConnecting = false;
        this.messageHandlers = new Map();
        this.connectionCallbacks = [];
    }

    connect() {
        if (this.isConnecting || (this.ws && this.ws.readyState === WebSocket.OPEN)) {
            return;
        }

        this.isConnecting = true;
        this.updateConnectionStatus('connecting');
        
        // Try to connect to Pipecat server WebSocket
        const wsUrl = `ws://0.0.0.0:8765`;
        console.log('🔌 Connecting to WebSocket:', wsUrl);
        console.log('ℹ️ Make sure the Pipecat server is running and accessible.');
        
        try {
            this.ws = new WebSocket(wsUrl);
            this.setupEventHandlers();
        } catch (error) {
            console.error('❌ WebSocket connection error:', error);
            this.handleConnectionError();
        }
    }

    setupEventHandlers() {
        this.ws.onopen = () => {
            console.log('✅ WebSocket connected successfully');
            this.isConnecting = false;
            this.reconnectAttempts = 0;
            this.updateConnectionStatus('online');
            
            // Send initial status request
            this.send({
                type: 'status_request',
                timestamp: new Date().toISOString()
            });
            
            // Notify connection callbacks
            this.connectionCallbacks.forEach(callback => callback(true));
        };

        this.ws.onmessage = (event) => {
            try {
                const message = JSON.parse(event.data);
                this.handleMessage(message);
            } catch (error) {
                console.error('❌ Failed to parse WebSocket message:', error);
            }
        };

        this.ws.onclose = (event) => {
            console.log('🔌 WebSocket connection closed:', event.code, event.reason);
            this.isConnecting = false;
            this.updateConnectionStatus('offline');
            
            // Notify connection callbacks
            this.connectionCallbacks.forEach(callback => callback(false));
            
            // Attempt to reconnect
            if (this.reconnectAttempts < this.maxReconnectAttempts) {
                this.scheduleReconnect();
            } else {
                console.error('❌ Max reconnection attempts reached');
                this.addLogEntry('ERROR', 'WebSocket connection failed after maximum attempts');
            }
        };

        this.ws.onerror = (error) => {
            console.error('❌ WebSocket error:', error);
            this.handleConnectionError();
        };
    }

    handleConnectionError() {
        this.isConnecting = false;
        this.updateConnectionStatus('offline');
        this.addLogEntry('ERROR', 'WebSocket connection failed');
        
        if (this.reconnectAttempts < this.maxReconnectAttempts) {
            this.scheduleReconnect();
        }
    }

    scheduleReconnect() {
        this.reconnectAttempts++;
        const delay = this.reconnectDelay * Math.pow(2, this.reconnectAttempts - 1);
        
        console.log(`🔄 Scheduling reconnect attempt ${this.reconnectAttempts} in ${delay}ms`);
        this.addLogEntry('INFO', `Reconnecting in ${delay / 1000}s (attempt ${this.reconnectAttempts})`);
        
        setTimeout(() => {
            this.connect();
        }, delay);
    }

    send(message) {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify(message));
            return true;
        } else {
            console.warn('⚠️ WebSocket not connected, cannot send message');
            return false;
        }
    }

    handleMessage(message) {
        const { type } = message;
        
        // Handle different message types
        switch (type) {
            case 'log':
                this.handleLogMessage(message);
                break;
            case 'status':
                this.handleStatusMessage(message);
                break;
            case 'stats':
                this.handleStatsMessage(message);
                break;
            case 'pipeline_status':
                this.handlePipelineStatus(message);
                break;
            case 'audio_data':
                this.handleAudioData(message);
                break;
            case 'transcription':
            case 'llm_response':
            case 'tts_started':
            case 'tts_completed':
            case 'audio':
            case 'error':
                // Handle Pipecat AI pipeline messages
                if (window.dashboard && window.dashboard.handlePipecatMessage) {
                    window.dashboard.handlePipecatMessage(message);
                }
                break;
            case 'pong':
                // Handle ping response
                break;
            case 'rustpbx_status_update':
                this.handleRustPBXStatusUpdate(message);
                break;
            default:
                console.log('📨 Received message:', message);
        }

        // Notify registered handlers
        if (this.messageHandlers.has(type)) {
            this.messageHandlers.get(type)(message);
        }
    }

    handleLogMessage(message) {
        const { level, text, timestamp } = message;
        this.addLogEntry(level.toUpperCase(), text, timestamp);
    }

    handleStatusMessage(message) {
        const { server_status, ai_services } = message;
        
        if (server_status) {
            this.updateServerInfo(server_status);
        }
        
        if (ai_services) {
            this.updateAIServiceStatus(ai_services);
        }
    }

    handleStatsMessage(message) {
        const { stats } = message;
        if (stats) {
            this.updateStatistics(stats);
        }
    }

    handlePipelineStatus(message) {
        const { pipeline } = message;
        if (pipeline) {
            this.updatePipelineStatus(pipeline);
        }
    }

    handleAudioData(message) {
        const { volume_level, waveform } = message;
        
        if (volume_level !== undefined) {
            this.updateVolumeLevel(volume_level);
        }
        
        if (waveform && window.audioVisualizer) {
            window.audioVisualizer.updateWaveform(waveform);
        }
    }

    addLogEntry(level, text, timestamp = null) {
        const logsOutput = document.getElementById('logs-output');
        if (!logsOutput) return;

        const logEntry = document.createElement('div');
        logEntry.className = 'log-entry';
        
        const timeStr = timestamp ? new Date(timestamp).toLocaleTimeString() : new Date().toLocaleTimeString();
        
        logEntry.innerHTML = `
            <span class="timestamp">[${timeStr}]</span>
            <span class="level ${level}">${level}</span>
            <span class="message">${text}</span>
        `;
        
        logsOutput.appendChild(logEntry);
        
        // Auto-scroll if enabled
        const autoScroll = document.getElementById('auto-scroll');
        if (autoScroll && autoScroll.checked) {
            logsOutput.scrollTop = logsOutput.scrollHeight;
        }
        
        // Limit log entries
        const maxLogs = 1000;
        if (logsOutput.children.length > maxLogs) {
            logsOutput.removeChild(logsOutput.firstChild);
        }
    }

    updateConnectionStatus(status) {
        const statusElement = document.getElementById('server-status');
        if (!statusElement) return;

        statusElement.className = `status-indicator ${status}`;
        
        const statusText = {
            'online': '🟢 Connected',
            'offline': '🔴 Disconnected',
            'connecting': '🟡 Connecting...'
        };
        
        statusElement.innerHTML = `<i class="fas fa-circle"></i> ${statusText[status] || status}`;
    }

    updateServerInfo(info) {
        // Update server information display
        console.log('📊 Server info:', info);
    }

    updateAIServiceStatus(services) {
        // Update AI service status indicators
        if (services.stt) {
            this.updateServiceStatus('stt-status', services.stt);
        }
        if (services.llm) {
            this.updateServiceStatus('llm-status', services.llm);
        }
        if (services.tts) {
            this.updateServiceStatus('tts-status', services.tts);
        }
    }

    updateServiceStatus(elementId, status) {
        const element = document.getElementById(elementId);
        if (!element) return;

        element.className = `step-status ${status}`;
        element.textContent = status.charAt(0).toUpperCase() + status.slice(1);
    }

    updateStatistics(stats) {
        // Update dashboard statistics
        this.updateStatElement('active-connections', stats.active_connections || 0);
        this.updateStatElement('messages-processed', stats.messages_processed || 0);
        this.updateStatElement('audio-frames', stats.audio_frames_processed || 0);
        this.updateStatElement('error-count', stats.errors || 0);
    }

    updateStatElement(elementId, value) {
        const element = document.getElementById(elementId);
        if (element) {
            element.textContent = value.toLocaleString();
        }
    }

    updatePipelineStatus(pipeline) {
        // Update pipeline visualization
        console.log('🔄 Pipeline status:', pipeline);
    }

    updateVolumeLevel(level) {
        const volumeElement = document.getElementById('volume-level');
        if (volumeElement) {
            const percentage = Math.min(100, Math.max(0, level * 100));
            volumeElement.style.width = `${percentage}%`;
        }
    }

    handleRustPBXStatusUpdate(message) {
        // Update RustPBX connection status
        const rustpbxStatus = document.getElementById('rustpbx-status');
        if (rustpbxStatus) {
            rustpbxStatus.textContent = message.rustpbx_connected ? 'Connected' : 'Not connected';
            rustpbxStatus.className = `status-value ${message.rustpbx_connected ? 'connected' : 'disconnected'}`;
        }
        
        // Update audio streaming status
        const audioStreamingStatus = document.getElementById('audio-streaming-status');
        if (audioStreamingStatus) {
            audioStreamingStatus.textContent = message.audio_streaming_active ? 'Active' : 'Inactive';
            audioStreamingStatus.className = `status-value ${message.audio_streaming_active ? 'active' : 'inactive'}`;
        }
        
        // Update active sessions
        const activeSessionsElement = document.getElementById('active-sessions');
        if (activeSessionsElement) {
            activeSessionsElement.textContent = message.active_sessions || 0;
        }
        
        // Log the status change
        this.addLogEntry('INFO', 
            `RustPBX: ${message.rustpbx_connected ? 'Connected' : 'Disconnected'}, ` +
            `Audio: ${message.audio_streaming_active ? 'Active' : 'Inactive'}, ` +
            `Sessions: ${message.active_sessions || 0}`
        );
    }

    // Register message handler
    onMessage(type, handler) {
        this.messageHandlers.set(type, handler);
    }

    // Register connection status callback
    onConnection(callback) {
        this.connectionCallbacks.push(callback);
    }

    // Ping server
    ping() {
        this.send({
            type: 'ping',
            timestamp: new Date().toISOString()
        });
    }

    // Request server status
    requestStatus() {
        this.send({
            type: 'status_request'
        });
    }

    // Request statistics
    requestStats() {
        this.send({
            type: 'stats_request'
        });
    }

    disconnect() {
        if (this.ws) {
            this.ws.close();
            this.ws = null;
        }
        this.updateConnectionStatus('offline');
    }
}

// Create global WebSocket manager instance
window.wsManager = new WebSocketManager();