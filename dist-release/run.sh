#!/bin/bash
DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ACTION=${1:-"start"}
CURR_USER=$(whoami)

install_service() {
    echo "⚙️ Installing systemd service..."
    sudo bash -c "cat <<SVC > /etc/systemd/system/axon-agent.service
[Unit]
Description=Axon Agent
After=network.target

[Service]
Type=simple
User=$CURR_USER
WorkingDirectory=$DEPLOY_DIR/core
Environment=MALLOC_ARENA_MAX=2
Environment=MALLOC_TRIM_THRESHOLD_=131072
ExecStart=$DEPLOY_DIR/core/axon
Restart=always
RestartSec=5
StandardOutput=append:$DEPLOY_DIR/agent.log
StandardError=append:$DEPLOY_DIR/agent.log

[Install]
WantedBy=multi-user.target
SVC"
    sudo systemctl daemon-reload
    sudo systemctl enable axon-agent
    echo "✅ Service installed and enabled."
}

case "$ACTION" in
    "--install") install_service ;;
    "start")
        if systemctl is-active --quiet axon-agent; then
            sudo systemctl restart axon-agent
        elif [ -f "/etc/systemd/system/axon-agent.service" ]; then
            sudo systemctl start axon-agent
        else
            pkill -f 'core/axon' || true; sleep 1
            cd "$DEPLOY_DIR/core" && MALLOC_ARENA_MAX=2 ./axon > "$DEPLOY_DIR/agent.log" 2>&1 &
            echo "⚠️ Started in background. Use './run.sh --install' for auto-restart."
        fi
        echo "📊 Logs: journalctl -u axon-agent -f   (or tail $DEPLOY_DIR/agent.log)"
        ;;
    "stop")    sudo systemctl stop axon-agent 2>/dev/null || true; pkill -f 'core/axon' || true ;;
    "restart") $0 stop; sleep 1; $0 start ;;
    "status")  systemctl status axon-agent ;;
    *) echo "Usage: ./run.sh [start|stop|restart|status|--install]"; exit 1 ;;
esac
