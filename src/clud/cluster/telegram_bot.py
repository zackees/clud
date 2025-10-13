"""
Telegram bot integration for CLUD-CLUSTER.

Provides remote control and monitoring of agents via Telegram chat interface.
Supports commands: /start, /list, /bind, /unbind, /stop, /tail, /status, /help.
"""

import logging
from datetime import datetime, timezone
from typing import Any
from uuid import UUID

from telegram import Update
from telegram.ext import (
    Application,
    CommandHandler,
    ContextTypes,
)

from . import database as db_module
from .database import AgentDB, Database
from .models import Agent, AgentStatus, BindingMode, Staleness, TelegramBinding

logger = logging.getLogger(__name__)


class TelegramBot:
    """
    Telegram bot for remote agent control.

    Integrates with CLUD-CLUSTER to provide chat-based interface for:
    - Listing agents
    - Binding chat to agent (for PTY output streaming)
    - Stopping agents
    - Viewing scrollback/tail
    - Agent status monitoring
    """

    def __init__(self, db: Database, token: str) -> None:
        """
        Initialize Telegram bot.

        Args:
            db: Database instance for agent/binding queries
            token: Telegram bot token from BotFather
        """
        self.db = db
        self.token = token
        self.application: Application[Any, Any, Any, Any, Any, Any] | None = None
        self._running = False

    async def start(self) -> None:
        """Start the Telegram bot (called during app lifespan)."""
        if not self.token:
            logger.warning("Telegram bot token not configured, bot disabled")
            return

        logger.info("Starting Telegram bot...")
        self.application = Application.builder().token(self.token).build()

        # Register command handlers
        self.application.add_handler(CommandHandler("start", self.cmd_start))
        self.application.add_handler(CommandHandler("help", self.cmd_help))
        self.application.add_handler(CommandHandler("list", self.cmd_list))
        self.application.add_handler(CommandHandler("bind", self.cmd_bind))
        self.application.add_handler(CommandHandler("unbind", self.cmd_unbind))
        self.application.add_handler(CommandHandler("status", self.cmd_status))
        self.application.add_handler(CommandHandler("tail", self.cmd_tail))
        self.application.add_handler(CommandHandler("stop", self.cmd_stop))

        # Start bot in background
        await self.application.initialize()
        await self.application.start()
        await self.application.updater.start_polling()
        self._running = True
        logger.info("Telegram bot started successfully")

    async def stop(self) -> None:
        """Stop the Telegram bot (called during app shutdown)."""
        if not self.application or not self._running:
            return

        logger.info("Stopping Telegram bot...")
        await self.application.updater.stop()
        await self.application.stop()
        await self.application.shutdown()
        self._running = False
        logger.info("Telegram bot stopped")

    async def send_message(self, chat_id: int, text: str) -> None:
        """
        Send a message to a Telegram chat.

        Args:
            chat_id: Telegram chat ID
            text: Message text (supports Markdown)
        """
        if not self.application:
            logger.warning("Cannot send message: bot not initialized")
            return

        try:
            await self.application.bot.send_message(chat_id=chat_id, text=text, parse_mode="Markdown")
        except Exception as e:
            logger.error(f"Failed to send Telegram message to {chat_id}: {e}")

    async def send_agent_output(self, agent_id: UUID, output: str) -> None:
        """
        Send agent PTY output to all bound Telegram chats.

        Args:
            agent_id: Agent UUID
            output: PTY output text (raw)
        """
        async with self.db.get_session() as session:
            # Find all active bindings for this agent
            bindings = await db_module.list_telegram_bindings(session, agent_id=agent_id)

            for binding in bindings:
                if binding.mode == BindingMode.ACTIVE.value:
                    # Format output as code block
                    formatted = f"```\n{output}\n```"
                    await self.send_message(binding.chat_id, formatted)

    # Command Handlers

    async def cmd_start(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /start command."""
        await update.message.reply_text("🤖 *CLUD-CLUSTER Bot*\n\nI help you monitor and control your clud agents remotely.\n\nUse /help to see available commands.")

    async def cmd_help(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /help command."""
        help_text = """
📚 *Available Commands*

*Monitoring*:
/list - List all agents
/status <agent_id> - Show agent details
/tail <agent_id> [lines] - Show recent output

*Control*:
/bind <agent_id> - Bind this chat to agent (get live output)
/unbind <agent_id> - Unbind chat from agent
/stop <agent_id> - Stop an agent

*Info*:
/help - Show this help message
/start - Introduction message

*Agent ID Format*: Use first 8 characters of UUID
Example: `/status a1b2c3d4`
"""
        await update.message.reply_text(help_text)

    async def cmd_list(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /list command - show all agents."""
        async with self.db.get_session() as session:
            agents_db = await db_module.list_agents(session)

            # Convert to Pydantic models
            agents = [self._db_agent_to_model(a) for a in agents_db]

            if not agents:
                await update.message.reply_text("No agents found.")
                return

            # Group by daemon/hostname
            by_host: dict[str, list[Agent]] = {}
            for agent in agents:
                if agent.hostname not in by_host:
                    by_host[agent.hostname] = []
                by_host[agent.hostname].append(agent)

            # Format output
            lines = ["*Agents by Host*\n"]
            for hostname, host_agents in by_host.items():
                lines.append(f"📍 *{hostname}*")
                for agent in host_agents:
                    status_emoji = self._status_emoji(agent.status, agent.staleness)
                    agent_id_short = str(agent.id)[:8]
                    lines.append(f"  {status_emoji} `{agent_id_short}` - {agent.command[:40]}")
                lines.append("")

            await update.message.reply_text("\n".join(lines))

    async def cmd_status(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /status command - show agent details."""
        if not context.args or len(context.args) == 0:
            await update.message.reply_text("Usage: /status <agent_id>\nExample: /status a1b2c3d4")
            return

        agent_id_prefix = context.args[0]
        agent = await self._find_agent_by_prefix(agent_id_prefix)

        if not agent:
            await update.message.reply_text(f"❌ Agent not found: `{agent_id_prefix}`")
            return

        # Format status message
        status_emoji = self._status_emoji(agent.status, agent.staleness)
        uptime = (datetime.now(timezone.utc) - agent.created_at).total_seconds()
        uptime_str = self._format_duration(int(uptime))

        status_text = f"""
{status_emoji} *Agent Status*

*ID*: `{agent.id}`
*Host*: {agent.hostname}
*Status*: {agent.status.upper()} ({agent.staleness.value})
*PID*: {agent.pid}
*Command*: `{agent.command}`
*CWD*: `{agent.cwd}`
*Uptime*: {uptime_str}

*Metrics*:
  CPU: {agent.metrics.cpu_percent:.1f}%
  Memory: {agent.metrics.memory_mb} MB
  PTY Sent: {agent.metrics.pty_bytes_sent} bytes
  PTY Recv: {agent.metrics.pty_bytes_received} bytes

*Last Heartbeat*: {agent.last_heartbeat.strftime("%Y-%m-%d %H:%M:%S")} UTC
"""
        await update.message.reply_text(status_text)

    async def cmd_bind(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /bind command - bind chat to agent for live output."""
        if not context.args or len(context.args) == 0:
            await update.message.reply_text("Usage: /bind <agent_id>\nExample: /bind a1b2c3d4")
            return

        agent_id_prefix = context.args[0]
        agent = await self._find_agent_by_prefix(agent_id_prefix)

        if not agent:
            await update.message.reply_text(f"❌ Agent not found: `{agent_id_prefix}`")
            return

        chat_id = update.effective_chat.id
        operator_id = update.effective_user.username or str(update.effective_user.id)

        # Create or update binding
        async with self.db.get_session() as session:
            # Check for existing binding
            existing = await db_module.get_telegram_binding_by_chat_and_agent(session, chat_id, agent.id)

            if existing:
                await update.message.reply_text(f"✅ Already bound to agent `{str(agent.id)[:8]}`")
                return

            # Create new binding
            binding = TelegramBinding(
                chat_id=chat_id,
                agent_id=agent.id,
                operator_id=operator_id,
                mode=BindingMode.ACTIVE,
            )
            await db_module.create_telegram_binding(session, binding)

        await update.message.reply_text(f"🔗 Bound to agent `{str(agent.id)[:8]}`\nYou will now receive live PTY output from this agent.\nUse /unbind {agent_id_prefix} to stop.")

    async def cmd_unbind(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /unbind command - unbind chat from agent."""
        if not context.args or len(context.args) == 0:
            await update.message.reply_text("Usage: /unbind <agent_id>\nExample: /unbind a1b2c3d4")
            return

        agent_id_prefix = context.args[0]
        agent = await self._find_agent_by_prefix(agent_id_prefix)

        if not agent:
            await update.message.reply_text(f"❌ Agent not found: `{agent_id_prefix}`")
            return

        chat_id = update.effective_chat.id

        async with self.db.get_session() as session:
            binding = await db_module.get_telegram_binding_by_chat_and_agent(session, chat_id, agent.id)

            if not binding:
                await update.message.reply_text(f"❌ Not bound to agent `{str(agent.id)[:8]}`")
                return

            await db_module.delete_telegram_binding(session, binding.id)

        await update.message.reply_text(f"🔓 Unbound from agent `{str(agent.id)[:8]}`")

    async def cmd_tail(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /tail command - show recent agent output."""
        if not context.args or len(context.args) == 0:
            await update.message.reply_text("Usage: /tail <agent_id> [lines]\nExample: /tail a1b2c3d4 50")
            return

        agent_id_prefix = context.args[0]
        lines = int(context.args[1]) if len(context.args) > 1 else 20

        agent = await self._find_agent_by_prefix(agent_id_prefix)
        if not agent:
            await update.message.reply_text(f"❌ Agent not found: `{agent_id_prefix}`")
            return

        # TODO: Implement scrollback retrieval via daemon
        # For now, send a placeholder message
        await update.message.reply_text(f"📜 Last {lines} lines from `{str(agent.id)[:8]}`:\n\n```\n(Scrollback retrieval not yet implemented)\n```\n\n💡 Use /bind to get live output")

    async def cmd_stop(self, update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
        """Handle /stop command - stop an agent."""
        if not context.args or len(context.args) == 0:
            await update.message.reply_text("Usage: /stop <agent_id>\nExample: /stop a1b2c3d4")
            return

        agent_id_prefix = context.args[0]
        agent = await self._find_agent_by_prefix(agent_id_prefix)

        if not agent:
            await update.message.reply_text(f"❌ Agent not found: `{agent_id_prefix}`")
            return

        # TODO: Send agent_stop intent to daemon via WebSocket
        # For now, send a placeholder message
        await update.message.reply_text(f"⏹️ Stop command for `{str(agent.id)[:8]}`:\n\n(Agent stop via Telegram not yet implemented)\n\n💡 Use the Web UI or REST API to stop agents")

    # Helper Methods

    async def _find_agent_by_prefix(self, prefix: str) -> Agent | None:
        """Find agent by UUID prefix (first 8 characters)."""
        async with self.db.get_session() as session:
            agents_db = await db_module.list_agents(session)
            for agent_db in agents_db:
                if str(agent_db.id).startswith(prefix):
                    return self._db_agent_to_model(agent_db)
            return None

    @staticmethod
    def _db_agent_to_model(agent_db: AgentDB) -> Agent:
        """Convert database AgentDB to Pydantic Agent model."""
        from .models import AgentMetrics

        return Agent(
            id=agent_db.id,
            daemon_id=agent_db.daemon_id,
            hostname=agent_db.hostname,
            pid=agent_db.pid,
            cwd=agent_db.cwd,
            command=agent_db.command,
            status=AgentStatus(agent_db.status),
            capabilities=agent_db.capabilities,
            created_at=agent_db.created_at,
            updated_at=agent_db.updated_at,
            last_heartbeat=agent_db.last_heartbeat,
            stopped_at=agent_db.stopped_at,
            staleness=Staleness(agent_db.staleness),
            daemon_reported_status=agent_db.daemon_reported_status,
            daemon_reported_at=agent_db.daemon_reported_at,
            metrics=AgentMetrics(**agent_db.metrics) if agent_db.metrics else AgentMetrics(),
        )

    @staticmethod
    def _status_emoji(status: AgentStatus, staleness: Staleness) -> str:
        """Get emoji for agent status and staleness."""
        if staleness == Staleness.DISCONNECTED:
            return "🔴"
        elif staleness == Staleness.STALE:
            return "🟡"
        elif status == AgentStatus.RUNNING:
            return "🟢"
        elif status == AgentStatus.ERROR:
            return "❌"
        elif status == AgentStatus.STOPPED:
            return "⏹️"
        else:
            return "⚪"

    @staticmethod
    def _format_duration(seconds: int) -> str:
        """Format duration in human-readable format."""
        if seconds < 60:
            return f"{seconds}s"
        elif seconds < 3600:
            return f"{seconds // 60}m {seconds % 60}s"
        elif seconds < 86400:
            hours = seconds // 3600
            minutes = (seconds % 3600) // 60
            return f"{hours}h {minutes}m"
        else:
            days = seconds // 86400
            hours = (seconds % 86400) // 3600
            return f"{days}d {hours}h"
