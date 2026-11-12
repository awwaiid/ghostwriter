// Ghostwriter Web Configuration Interface

class GhostwriterConfig {
    constructor() {
        this.apiBase = '/api';
        this.form = document.getElementById('config-form');
        this.status = document.getElementById('status');
        this.loadBtn = document.getElementById('load-btn');

        this.init();
    }

    init() {
        this.form.addEventListener('submit', (e) => this.handleSubmit(e));
        this.loadBtn.addEventListener('click', () => this.loadConfig());

        // Load config on page load
        this.loadConfig();

        // Start status polling
        this.startStatusPolling();
    }

    async loadConfig() {
        try {
            this.showStatus('Loading configuration...', 'info');

            const response = await fetch(`${this.apiBase}/config`);
            if (!response.ok) {
                throw new Error(`Failed to load config: ${response.statusText}`);
            }

            const config = await response.json();
            this.populateForm(config);
            this.showStatus('Configuration loaded successfully', 'success');

            // Hide success message after 3 seconds
            setTimeout(() => this.hideStatus(), 3000);
        } catch (error) {
            console.error('Failed to load config:', error);
            this.showStatus(`Error loading configuration: ${error.message}`, 'error');
        }
    }

    async handleSubmit(e) {
        e.preventDefault();

        try {
            this.showStatus('Saving configuration...', 'info');

            const formData = new FormData(this.form);
            const config = this.formDataToConfig(formData);

            const response = await fetch(`${this.apiBase}/config`, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify(config)
            });

            if (!response.ok) {
                const errorText = await response.text();
                throw new Error(`Failed to save config: ${errorText}`);
            }

            this.showStatus('Configuration saved successfully', 'success');

            // Hide success message after 3 seconds
            setTimeout(() => this.hideStatus(), 3000);
        } catch (error) {
            console.error('Failed to save config:', error);
            this.showStatus(`Error saving configuration: ${error.message}`, 'error');
        }
    }

    populateForm(config) {
        // Text inputs and selects
        const textFields = [
            'model', 'prompt', 'engine', 'engine_base_url', 'engine_api_key',
            'trigger_corner', 'pen', 'log_level', 'thinking_tokens',
            'input_png', 'output_file', 'model_output_file',
            'save_screenshot', 'save_bitmap'
        ];

        textFields.forEach(field => {
            const element = document.getElementById(field);
            if (element && config[field] !== undefined && config[field] !== null) {
                element.value = config[field];
            }
        });

        // Checkboxes
        const boolFields = [
            'no_submit', 'no_draw', 'no_svg', 'no_keyboard', 'no_draw_progress', 'no_select_pen',
            'no_loop', 'no_trigger', 'apply_segmentation', 'web_search', 'thinking'
        ];

        boolFields.forEach(field => {
            const element = document.getElementById(field);
            if (element && config[field] !== undefined) {
                element.checked = config[field];
            }
        });
    }

    formDataToConfig(formData) {
        const config = {};

        // Text fields
        const textFields = [
            'model', 'prompt', 'engine', 'engine_base_url', 'engine_api_key',
            'trigger_corner', 'pen', 'log_level',
            'input_png', 'output_file', 'model_output_file',
            'save_screenshot', 'save_bitmap'
        ];

        textFields.forEach(field => {
            const value = formData.get(field);
            if (value && value.trim() !== '') {
                config[field] = value.trim();
            } else {
                config[field] = null;
            }
        });

        // Number fields
        const thinkingTokens = formData.get('thinking_tokens');
        if (thinkingTokens && thinkingTokens.trim() !== '') {
            config.thinking_tokens = parseInt(thinkingTokens, 10);
        } else {
            config.thinking_tokens = 5000; // Default value
        }

        // Boolean fields
        const boolFields = [
            'no_submit', 'no_draw', 'no_svg', 'no_keyboard', 'no_draw_progress', 'no_select_pen',
            'no_loop', 'no_trigger', 'apply_segmentation', 'web_search', 'thinking'
        ];

        boolFields.forEach(field => {
            config[field] = formData.has(field);
        });

        return config;
    }

    showStatus(message, type) {
        this.status.textContent = message;
        this.status.className = `status ${type}`;
    }

    hideStatus() {
        this.status.style.display = 'none';
    }

    startStatusPolling() {
        // Poll status every 2 seconds
        this.loadStatus();
        setInterval(() => this.loadStatus(), 2000);
    }

    async loadStatus() {
        try {
            const response = await fetch(`${this.apiBase}/status`);
            if (!response.ok) {
                console.warn('Failed to load status:', response.statusText);
                return;
            }

            const status = await response.json();
            this.updateStatusDisplay(status);
        } catch (error) {
            console.warn('Failed to load status:', error);
        }
    }

    updateStatusDisplay(status) {
        // Update running status
        const runningStatusEl = document.getElementById('running-status');
        if (status.running) {
            if (status.processing) {
                runningStatusEl.textContent = 'Processing';
                runningStatusEl.className = 'status-value processing';
            } else if (status.waiting_for_trigger) {
                runningStatusEl.textContent = 'Waiting for trigger';
                runningStatusEl.className = 'status-value waiting';
            } else {
                runningStatusEl.textContent = 'Running';
                runningStatusEl.className = 'status-value running';
            }
        } else {
            runningStatusEl.textContent = 'Stopped';
            runningStatusEl.className = 'status-value';
        }

        // Show error if present
        if (status.error) {
            runningStatusEl.textContent = `Error: ${status.error}`;
            runningStatusEl.className = 'status-value error';
        }

        // Update other status fields
        document.getElementById('current-model').textContent = status.current_model || 'N/A';
        document.getElementById('current-prompt').textContent = status.current_prompt || 'N/A';
        document.getElementById('executions-count').textContent = status.executions_count || '0';
        document.getElementById('last-activity').textContent = status.last_activity || 'None';

        // Format uptime
        const uptimeEl = document.getElementById('uptime');
        if (status.uptime_seconds) {
            const hours = Math.floor(status.uptime_seconds / 3600);
            const minutes = Math.floor((status.uptime_seconds % 3600) / 60);
            const seconds = status.uptime_seconds % 60;
            uptimeEl.textContent = `${hours}h ${minutes}m ${seconds}s`;
        } else {
            uptimeEl.textContent = '0s';
        }
    }
}

// Initialize the app when DOM is loaded
document.addEventListener('DOMContentLoaded', () => {
    new GhostwriterConfig();
});