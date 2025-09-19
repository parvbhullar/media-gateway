/**
 * Main dashboard controller for Pipecat Media Server
 */

class Dashboard {
    constructor() {
        this.isLogsPaused = false;
        this.statsUpdateInterval = null;
        this.pipelineState = {
            asr: { status: 'idle', lastText: '' },
            llm: { status: 'idle', lastResponse: '' },
            tts: { status: 'idle', isGenerating: false }
        };
        this.setupEventListeners();
        this.initialize();
    }

    setupEventListeners() {
        // Logs control buttons
        document.getElementById('clear-logs')?.addEventListener('click', () => {
            this.clearLogs();
        });
        
        document.getElementById('pause-logs')?.addEventListener('click', () => {
            this.toggleLogsPause();
        });
        
        // Auto-scroll checkbox
        document.getElementById('auto-scroll')?.addEventListener('change', (e) => {
            const logsOutput = document.getElementById('logs-output');
            if (e.target.checked && logsOutput) {
                logsOutput.scrollTop = logsOutput.scrollHeight;
            }
        });
        
        // Window events
        window.addEventListener('beforeunload', () => {
            this.cleanup();
        });
        
        // Keyboard shortcuts
        document.addEventListener('keydown', (e) => {
            this.handleKeyboardShortcuts(e);
        });
    }

    async initialize() {
        try {
            // Connect to WebSocket
            this.connectWebSocket();
            
            // Start periodic updates
            this.startPeriodicUpdates();
            
            // Initial server health check
            await this.checkServerHealth();
            
            wsManager.addLogEntry('INFO', 'Dashboard initialized successfully');
            
        } catch (error) {
            console.error('❌ Dashboard initialization failed:', error);
            wsManager.addLogEntry('ERROR', `Dashboard initialization failed: ${error.message}`);
        }
    }

    connectWebSocket() {
        // Connect to Pipecat server WebSocket
        wsManager.connect();
        
        // Setup connection status handler
        wsManager.onConnection((connected) => {
            if (connected) {
                this.onWebSocketConnected();
            } else {
                this.onWebSocketDisconnected();
            }
        });
        
        // Setup custom message handlers
        this.setupMessageHandlers();
    }

    setupMessageHandlers() {
        // Handle pipeline status updates
        wsManager.onMessage('pipeline_update', (message) => {
            this.updatePipelineVisualization(message.pipeline);
        });
        
        // Handle real-time statistics
        wsManager.onMessage('real_time_stats', (message) => {
            this.updateRealTimeStats(message.stats);
        });
        
        // Handle AI service status
        wsManager.onMessage('ai_service_status', (message) => {
            this.updateAIServiceStatus(message.services);
        });
        
        // Handle connection events
        wsManager.onMessage('connection_event', (message) => {
            this.handleConnectionEvent(message);
        });
    }

    onWebSocketConnected() {
        // Request initial status
        wsManager.requestStatus();
        wsManager.requestStats();
        
        // Enable real-time updates
        this.enableRealTimeUpdates();
        
        wsManager.addLogEntry('SUCCESS', 'Connected to Pipecat server');
    }

    onWebSocketDisconnected() {
        // Disable real-time updates
        this.disableRealTimeUpdates();
        
        // Reset status indicators
        this.resetStatusIndicators();
        
        wsManager.addLogEntry('WARNING', 'Disconnected from Pipecat server');
    }

    enableRealTimeUpdates() {
        // Send periodic ping and stats requests
        if (this.statsUpdateInterval) {
            clearInterval(this.statsUpdateInterval);
        }
        
        this.statsUpdateInterval = setInterval(() => {
            wsManager.ping();
            wsManager.requestStats();
        }, 5000); // Update every 5 seconds
    }

    disableRealTimeUpdates() {
        if (this.statsUpdateInterval) {
            clearInterval(this.statsUpdateInterval);
            this.statsUpdateInterval = null;
        }
    }

    async checkServerHealth() {
        try {
            const response = await fetch('/health');
            const healthData = await response.json();
            
            if (healthData.status === 'healthy') {
                wsManager.addLogEntry('SUCCESS', 'Server health check passed');
                this.updateHealthStatus(healthData);
            } else {
                wsManager.addLogEntry('WARNING', `Server health: ${healthData.status}`);
            }
        } catch (error) {
            wsManager.addLogEntry('ERROR', `Health check failed: ${error.message}`);
        }
    }

    updateHealthStatus(healthData) {
        // Update statistics with health data
        if (healthData.active_rooms !== undefined) {
            this.updateStatElement('active-connections', healthData.active_connections || 0);
        }
    }

    updatePipelineVisualization(pipeline) {
        // Update pipeline step indicators
        if (pipeline.stt) {
            this.updatePipelineStep('stt', pipeline.stt);
        }
        if (pipeline.llm) {
            this.updatePipelineStep('llm', pipeline.llm);
        }
        if (pipeline.tts) {
            this.updatePipelineStep('tts', pipeline.tts);
        }
    }

    updatePipelineStep(service, status) {
        const element = document.getElementById(`${service}-status`);
        if (element) {
            element.className = `step-status ${status.status}`;
            element.textContent = status.status.charAt(0).toUpperCase() + status.status.slice(1);
            
            // Add processing time if available
            if (status.processing_time) {
                element.title = `Processing time: ${status.processing_time}ms`;
            }
        }
    }

    updateRealTimeStats(stats) {
        // Update all statistics counters
        if (stats.active_connections !== undefined) {
            this.updateStatElement('active-connections', stats.active_connections);
        }
        if (stats.messages_processed !== undefined) {
            this.updateStatElement('messages-processed', stats.messages_processed);
        }
        if (stats.audio_frames_processed !== undefined) {
            this.updateStatElement('audio-frames', stats.audio_frames_processed);
        }
        if (stats.errors !== undefined) {
            this.updateStatElement('error-count', stats.errors);
        }
    }

    updateStatElement(elementId, value) {
        const element = document.getElementById(elementId);
        if (element) {
            // Animate counter updates
            this.animateCounter(element, parseInt(element.textContent.replace(/,/g, '')) || 0, value);
        }
    }

    animateCounter(element, startValue, endValue) {
        const duration = 500; // Animation duration in ms
        const startTime = Date.now();
        
        const updateCounter = () => {
            const elapsed = Date.now() - startTime;
            const progress = Math.min(elapsed / duration, 1);
            
            const currentValue = Math.floor(startValue + (endValue - startValue) * progress);
            element.textContent = currentValue.toLocaleString();
            
            if (progress < 1) {
                requestAnimationFrame(updateCounter);
            }
        };
        
        updateCounter();
    }

    updateAIServiceStatus(services) {
        Object.keys(services).forEach(service => {
            const status = services[service];
            this.updateServiceIndicator(service, status);
        });
    }

    updateServiceIndicator(service, status) {
        const elementId = `${service}-status`;
        const element = document.getElementById(elementId);
        
        if (element) {
            element.className = `step-status ${status.status}`;
            element.textContent = status.status.charAt(0).toUpperCase() + status.status.slice(1);
            
            // Update tooltip with additional info
            if (status.last_used) {
                element.title = `Last used: ${new Date(status.last_used).toLocaleTimeString()}`;
            }
        }
    }

    handleConnectionEvent(message) {
        const { event_type, client_id, details } = message;
        
        switch (event_type) {
            case 'client_connected':
                wsManager.addLogEntry('INFO', `Client connected: ${client_id}`);
                break;
            case 'client_disconnected':
                wsManager.addLogEntry('INFO', `Client disconnected: ${client_id}`);
                break;
            case 'rustpbx_connected':
                wsManager.addLogEntry('SUCCESS', 'RustPBX connected to Pipecat');
                break;
            case 'rustpbx_disconnected':
                wsManager.addLogEntry('WARNING', 'RustPBX disconnected from Pipecat');
                break;
            case 'audio_stream_started':
                wsManager.addLogEntry('SUCCESS', `Audio stream started: ${details}`);
                break;
            case 'audio_stream_stopped':
                wsManager.addLogEntry('INFO', `Audio stream stopped: ${details}`);
                break;
        }
    }

    clearLogs() {
        const logsOutput = document.getElementById('logs-output');
        if (logsOutput) {
            logsOutput.innerHTML = '';
            wsManager.addLogEntry('INFO', 'Logs cleared');
        }
    }

    toggleLogsPause() {
        this.isLogsPaused = !this.isLogsPaused;
        const pauseBtn = document.getElementById('pause-logs');
        
        if (pauseBtn) {
            if (this.isLogsPaused) {
                pauseBtn.innerHTML = '<i class="fas fa-play"></i> Resume';
                pauseBtn.classList.remove('btn-info');
                pauseBtn.classList.add('btn-success');
            } else {
                pauseBtn.innerHTML = '<i class="fas fa-pause"></i> Pause';
                pauseBtn.classList.remove('btn-success');
                pauseBtn.classList.add('btn-info');
            }
        }
        
        wsManager.addLogEntry('INFO', `Logs ${this.isLogsPaused ? 'paused' : 'resumed'}`);
    }

    resetStatusIndicators() {
        // Reset all status indicators to unknown state
        const statusElements = ['stt-status', 'llm-status', 'tts-status'];
        statusElements.forEach(elementId => {
            const element = document.getElementById(elementId);
            if (element) {
                element.className = 'step-status unknown';
                element.textContent = 'Unknown';
                element.title = '';
            }
        });
        
        // Reset statistics
        const statElements = ['active-connections', 'messages-processed', 'audio-frames', 'error-count'];
        statElements.forEach(elementId => {
            const element = document.getElementById(elementId);
            if (element) {
                element.textContent = '0';
            }
        });
    }

    handleKeyboardShortcuts(e) {
        // Ctrl/Cmd + combinations
        if (e.ctrlKey || e.metaKey) {
            switch (e.key) {
                case 'l':
                    e.preventDefault();
                    this.clearLogs();
                    break;
                case 'p':
                    e.preventDefault();
                    this.toggleLogsPause();
                    break;
                case 'r':
                    e.preventDefault();
                    this.refreshDashboard();
                    break;
            }
        }
        
        // Function keys
        switch (e.key) {
            case 'F5':
                e.preventDefault();
                this.refreshDashboard();
                break;
        }
    }

    refreshDashboard() {
        wsManager.addLogEntry('INFO', 'Refreshing dashboard...');
        
        // Reconnect WebSocket if needed
        if (!wsManager.ws || wsManager.ws.readyState !== WebSocket.OPEN) {
            wsManager.connect();
        }
        
        // Request fresh data
        wsManager.requestStatus();
        wsManager.requestStats();
        
        // Check server health
        this.checkServerHealth();
    }

    startPeriodicUpdates() {
        // Start background tasks
        this.enableRealTimeUpdates();
        
        // Monitor WebSocket connection
        setInterval(() => {
            if (!wsManager.ws || wsManager.ws.readyState !== WebSocket.OPEN) {
                wsManager.connect();
            }
        }, 10000); // Check every 10 seconds
    }

    cleanup() {
        // Clean up resources when page unloads
        this.disableRealTimeUpdates();
        
        if (window.webrtcManager) {
            window.webrtcManager.stopAudioStream();
            window.webrtcManager.disconnectFromRustPBX();
        }
        
        if (window.audioVisualizer) {
            window.audioVisualizer.stopAnimation();
        }
        
        wsManager.disconnect();
    }

    // Export logs functionality
    exportLogs() {
        const logsOutput = document.getElementById('logs-output');
        if (!logsOutput) return;
        
        const logEntries = Array.from(logsOutput.children);
        const logText = logEntries.map(entry => entry.textContent).join('\n');
        
        const blob = new Blob([logText], { type: 'text/plain' });
        const url = URL.createObjectURL(blob);
        
        const a = document.createElement('a');
        a.href = url;
        a.download = `pipecat-logs-${new Date().toISOString().slice(0, 19).replace(/:/g, '-')}.txt`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        
        URL.revokeObjectURL(url);
        wsManager.addLogEntry('INFO', 'Logs exported successfully');
    }

    // Pipeline management methods
    updatePipelineStep(step, status, content = null) {
        const stepElement = document.getElementById(`${step}-step`);
        const statusElement = document.getElementById(`${step}-status`);
        
        if (stepElement && statusElement) {
            // Update step styling
            stepElement.classList.remove('active', 'processing', 'idle');
            stepElement.classList.add(status);
            
            // Update status indicator
            const statusDot = statusElement.querySelector('.status-dot');
            const statusText = statusElement.querySelector('.status-text');
            
            if (statusDot) {
                statusDot.classList.remove('idle', 'active', 'processing', 'error');
                statusDot.classList.add(status);
            }
            
            if (statusText) {
                const statusMessages = {
                    idle: 'Idle',
                    processing: 'Processing...',
                    active: 'Active',
                    error: 'Error'
                };
                statusText.textContent = statusMessages[status] || status;
            }
            
            // Update content if provided
            if (content) {
                this.updatePipelineContent(step, content);
            }
        }
        
        // Update internal state
        this.pipelineState[step].status = status;
    }
    
    updatePipelineContent(step, content) {
        const contentElements = {
            asr: 'latest-transcription',
            llm: 'latest-response', 
            tts: 'audio-generation-status'
        };
        
        const elementId = contentElements[step];
        const element = document.getElementById(elementId);
        
        if (element) {
            element.textContent = content;
            
            // Add animation effect
            element.style.opacity = '0.5';
            setTimeout(() => {
                element.style.opacity = '1';
            }, 100);
        }
        
        // Update internal state
        if (step === 'asr') {
            this.pipelineState.asr.lastText = content;
        } else if (step === 'llm') {
            this.pipelineState.llm.lastResponse = content;
        }
    }
    
    handlePipecatMessage(data) {
        if (!data.type) return;
        
        switch (data.type) {
            case 'transcription':
                if (data.is_final) {
                    this.updatePipelineStep('asr', 'active', data.text);
                    wsManager.addLogEntry('INFO', `[ASR] Final: ${data.text}`);
                } else {
                    this.updatePipelineStep('asr', 'processing', data.text);
                }
                break;
                
            case 'llm_response':
                if (data.is_complete) {
                    this.updatePipelineStep('llm', 'active', data.text);
                    wsManager.addLogEntry('INFO', `[LLM] Complete: ${data.text}`);
                } else {
                    this.updatePipelineStep('llm', 'processing', data.text);
                }
                break;
                
            case 'tts_started':
                this.updatePipelineStep('tts', 'processing', `Generating audio for: "${data.text}"`);
                wsManager.addLogEntry('INFO', `[TTS] Started: ${data.text}`);
                break;
                
            case 'tts_completed':
                this.updatePipelineStep('tts', 'active', 'Audio generation completed');
                wsManager.addLogEntry('INFO', `[TTS] Completed: ${data.text}`);
                break;
                
            case 'audio':
                // Audio frame received from RustPBX
                this.updateIntegrationStatus('audio-streaming-status', 'Active');
                if (this.pipelineState.asr.status === 'idle') {
                    this.updatePipelineStep('asr', 'processing', 'Processing incoming audio...');
                }
                break;
                
            case 'error':
                const errorStep = this.determineErrorStep(data.message);
                if (errorStep) {
                    this.updatePipelineStep(errorStep, 'error', `Error: ${data.message}`);
                }
                wsManager.addLogEntry('ERROR', `[Pipeline] ${data.message}`);
                break;
        }
    }
    
    updateIntegrationStatus(elementId, status) {
        const element = document.getElementById(elementId);
        if (element) {
            element.textContent = status;
            element.className = `status-value ${status.toLowerCase().replace(' ', '-')}`;
        }
    }
    
    determineErrorStep(errorMessage) {
        const message = errorMessage.toLowerCase();
        if (message.includes('speech') || message.includes('transcription') || message.includes('asr')) {
            return 'asr';
        } else if (message.includes('llm') || message.includes('openai') || message.includes('gpt')) {
            return 'llm';
        } else if (message.includes('tts') || message.includes('synthesis') || message.includes('voice')) {
            return 'tts';
        }
        return null;
    }
    
    resetPipeline() {
        this.updatePipelineStep('asr', 'idle', 'Waiting for audio input...');
        this.updatePipelineStep('llm', 'idle', 'Waiting for user input...');
        this.updatePipelineStep('tts', 'idle', 'No audio being generated');
        this.updateIntegrationStatus('audio-streaming-status', 'Inactive');
    }

    // Test AI pipeline functionality
    async testAIPipeline() {
        try {
            wsManager.addLogEntry('INFO', 'Testing AI pipeline...');
            
            // Send test message to Pipecat server
            const testMessage = {
                type: 'test_pipeline',
                text: 'Hello, this is a test message for the AI pipeline.',
                timestamp: new Date().toISOString()
            };
            
            const success = wsManager.send(testMessage);
            if (success) {
                wsManager.addLogEntry('SUCCESS', 'AI pipeline test message sent');
            } else {
                wsManager.addLogEntry('ERROR', 'Failed to send AI pipeline test message');
            }
            
        } catch (error) {
            wsManager.addLogEntry('ERROR', `AI pipeline test failed: ${error.message}`);
        }
    }
}

// Initialize dashboard when DOM is loaded
document.addEventListener('DOMContentLoaded', () => {
    window.dashboard = new Dashboard();
    
    // Add some welcome messages
    setTimeout(() => {
        wsManager.addLogEntry('INFO', 'Pipecat Media Server Dashboard loaded');
        wsManager.addLogEntry('INFO', 'Ready to monitor AI pipeline and WebRTC streaming');
        wsManager.addLogEntry('INFO', 'Use Ctrl+L to clear logs, Ctrl+P to pause/resume');
    }, 500);
});